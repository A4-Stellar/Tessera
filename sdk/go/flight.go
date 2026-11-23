// Package tessera provides a client library for interacting with the Tessera API,
// including REST endpoints, WebSocket event streams, and Apache Arrow Flight gRPC streaming.
package tessera

import (
	"bytes"
	"context"
	"fmt"
	"io"
	"net/url"
	"strconv"
	"sync"
	"time"

	"github.com/apache/arrow-go/v18/arrow"
	"github.com/apache/arrow-go/v18/arrow/flight"
	"github.com/apache/arrow-go/v18/arrow/ipc"
	"go.opentelemetry.io/otel"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/credentials/insecure"
	"google.golang.org/grpc/metadata"
	"google.golang.org/grpc/status"
)

// FlightDescriptor describes the data to fetch via Arrow Flight.
type FlightDescriptor = flight.FlightDescriptor

// FlightClient wraps Apache Arrow Flight gRPC streaming for high-throughput
// columnar data export and retrieval.
type FlightClient struct {
	conn       *grpc.ClientConn
	flightCl   flight.Client
	authToken  string
	maxRetries int
	initBack   time.Duration
	maxBack    time.Duration
	mu         sync.RWMutex
}

// FlightOption configures a FlightClient.
type FlightOption func(*flightConfig)

type flightConfig struct {
	dialOptions    []grpc.DialOption
	authToken      string
	maxRetries     int
	initialBackoff time.Duration
	maxBackoff     time.Duration
}

// WithFlightAuthToken sets the bearer authentication token for Flight gRPC calls.
func WithFlightAuthToken(token string) FlightOption {
	return func(c *flightConfig) {
		c.authToken = token
	}
}

// WithFlightDialOptions appends custom gRPC dial options.
func WithFlightDialOptions(opts ...grpc.DialOption) FlightOption {
	return func(c *flightConfig) {
		c.dialOptions = append(c.dialOptions, opts...)
	}
}

// WithFlightRetry sets the retry policy for UNAVAILABLE gRPC status codes.
func WithFlightRetry(maxRetries int, initialBackoff, maxBackoff time.Duration) FlightOption {
	return func(c *flightConfig) {
		c.maxRetries = maxRetries
		c.initialBackoff = initialBackoff
		c.maxBackoff = maxBackoff
	}
}

// NewFlightClient creates and initializes a FlightClient connected to the target address.
func NewFlightClient(target string, opts ...FlightOption) (*FlightClient, error) {
	cfg := flightConfig{
		maxRetries:     3,
		initialBackoff: 100 * time.Millisecond,
		maxBackoff:     2 * time.Second,
	}
	for _, opt := range opts {
		opt(&cfg)
	}

	unaryInterceptors := []grpc.UnaryClientInterceptor{
		TraceUnaryClientInterceptor(),
		AuthUnaryClientInterceptor(cfg.authToken),
		RetryUnaryClientInterceptor(cfg.maxRetries, cfg.initialBackoff, cfg.maxBackoff),
	}
	streamInterceptors := []grpc.StreamClientInterceptor{
		TraceStreamClientInterceptor(),
		AuthStreamClientInterceptor(cfg.authToken),
		RetryStreamClientInterceptor(cfg.maxRetries, cfg.initialBackoff, cfg.maxBackoff),
	}

	dialOpts := []grpc.DialOption{
		grpc.WithChainUnaryInterceptor(unaryInterceptors...),
		grpc.WithChainStreamInterceptor(streamInterceptors...),
		grpc.WithTransportCredentials(insecure.NewCredentials()),
	}
	dialOpts = append(dialOpts, cfg.dialOptions...)

	conn, err := grpc.Dial(target, dialOpts...)
	if err != nil {
		return nil, fmt.Errorf("failed to dial flight service at %s: %w", target, err)
	}

	flightCl := flight.NewClientFromConn(conn, nil)

	return &FlightClient{
		conn:       conn,
		flightCl:   flightCl,
		authToken:  cfg.authToken,
		maxRetries: cfg.maxRetries,
		initBack:   cfg.initialBackoff,
		maxBack:    cfg.maxBackoff,
	}, nil
}

// FlightClient returns a new FlightClient configured to connect to the Flight gRPC service.
// If target is empty, it is inferred from the Client's BaseURL host.
func (c *Client) FlightClient(target string, opts ...FlightOption) (*FlightClient, error) {
	if target == "" {
		u, err := url.Parse(c.config.BaseURL)
		if err == nil && u.Host != "" {
			target = u.Host
		} else {
			target = "localhost:50051"
		}
	}
	if c.config.AuthToken != "" {
		opts = append([]FlightOption{WithFlightAuthToken(c.config.AuthToken)}, opts...)
	}
	return NewFlightClient(target, opts...)
}

// NewFlightClientFromConn creates a FlightClient using an existing gRPC ClientConn.
func NewFlightClientFromConn(conn *grpc.ClientConn, authToken string) *FlightClient {
	return &FlightClient{
		conn:      conn,
		flightCl:  flight.NewClientFromConn(conn, nil),
		authToken: authToken,
	}
}

// Close closes the underlying gRPC connection.
func (fc *FlightClient) Close() error {
	fc.mu.Lock()
	defer fc.mu.Unlock()
	if fc.flightCl != nil {
		_ = fc.flightCl.Close()
	}
	if fc.conn != nil {
		return fc.conn.Close()
	}
	return nil
}

// ExportAssetArrow streams record batches for the GET /v1/assets/:id/export?format=arrow endpoint.
func (fc *FlightClient) ExportAssetArrow(ctx context.Context, assetID uint64) (*FlightDataStream, error) {
	desc := &flight.FlightDescriptor{
		Type: flight.DescriptorPATH,
		Path: []string{"v1", "assets", strconv.FormatUint(assetID, 10), "export"},
		Cmd:  []byte(fmt.Sprintf("/v1/assets/%d/export?format=arrow", assetID)),
	}
	return fc.DoGet(ctx, desc)
}

// DoGet executes a Flight DoGet RPC using the descriptor and returns a streaming FlightDataStream.
// Record batches are downloaded in a streaming fashion without loading the entire response into memory.
// It automatically retries on codes.Unavailable with exponential backoff.
func (fc *FlightClient) DoGet(ctx context.Context, desc *flight.FlightDescriptor) (*FlightDataStream, error) {
	ticket := &flight.Ticket{
		Ticket: desc.Cmd,
	}
	if len(ticket.Ticket) == 0 && len(desc.Path) > 0 {
		ticket.Ticket = []byte(desc.Path[len(desc.Path)-1])
	}

	var lastErr error
	backoff := fc.initBack
	if backoff <= 0 {
		backoff = 100 * time.Millisecond
	}
	maxBack := fc.maxBack
	if maxBack <= 0 {
		maxBack = 2 * time.Second
	}
	maxRetries := fc.maxRetries

	for attempt := 0; attempt <= maxRetries; attempt++ {
		if err := ctx.Err(); err != nil {
			return nil, err
		}

		stream, err := fc.flightCl.DoGet(ctx, ticket)
		if err != nil {
			lastErr = fmt.Errorf("flight DoGet failed: %w", err)
			if status.Code(err) == codes.Unavailable && attempt < maxRetries {
				select {
				case <-ctx.Done():
					return nil, ctx.Err()
				case <-time.After(backoff):
					backoff = minDuration(backoff*2, maxBack)
					continue
				}
			}
			return nil, lastErr
		}

		reader, err := flight.NewRecordReader(stream)
		if err != nil {
			lastErr = fmt.Errorf("failed to create flight record reader: %w", err)
			if status.Code(err) == codes.Unavailable && attempt < maxRetries {
				select {
				case <-ctx.Done():
					return nil, ctx.Err()
				case <-time.After(backoff):
					backoff = minDuration(backoff*2, maxBack)
					continue
				}
			}
			return nil, lastErr
		}

		return &FlightDataStream{
			stream: stream,
			reader: reader,
		}, nil
	}

	return nil, lastErr
}

// FlightDataStream wraps an active Arrow Flight record stream.
type FlightDataStream struct {
	stream flight.FlightService_DoGetClient
	reader *flight.Reader
	mu     sync.Mutex
	closed bool
}

// Next advances the stream to the next record batch.
// It returns false when there are no more batches or an error occurred.
func (s *FlightDataStream) Next() bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.closed || s.reader == nil {
		return false
	}
	return s.reader.Next()
}

// Record returns the current Arrow record batch.
func (s *FlightDataStream) Record() arrow.Record {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.reader == nil {
		return nil
	}
	return s.reader.Record()
}

// Schema returns the schema of the record batches in the stream.
func (s *FlightDataStream) Schema() *arrow.Schema {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.reader == nil {
		return nil
	}
	return s.reader.Schema()
}

// Err returns any error encountered during streaming.
func (s *FlightDataStream) Err() error {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.reader == nil {
		return nil
	}
	return s.reader.Err()
}

// Close closes the stream and releases Arrow resources.
func (s *FlightDataStream) Close() error {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.closed {
		return nil
	}
	s.closed = true
	if s.reader != nil {
		s.reader.Release()
	}
	return nil
}

// BatchReader wraps a FlightDataStream and satisfies io.Reader,
// enabling direct streaming of Arrow IPC batches to a Parquet writer or file
// without buffering the entire dataset in memory.
type BatchReader struct {
	stream     *FlightDataStream
	buf        bytes.Buffer
	ipcWriter  *ipc.Writer
	initWriter bool
	closed     bool
}

// NewBatchReader creates a new BatchReader for the given FlightDataStream.
func NewBatchReader(stream *FlightDataStream) *BatchReader {
	return &BatchReader{
		stream: stream,
	}
}

// NewBatchReader creates an io.Reader from this FlightDataStream.
func (s *FlightDataStream) NewBatchReader() *BatchReader {
	return NewBatchReader(s)
}

// Read implements io.Reader for streaming record batches.
func (br *BatchReader) Read(p []byte) (n int, err error) {
	if br.closed {
		return 0, io.EOF
	}

	for br.buf.Len() == 0 {
		if !br.initWriter {
			schema := br.stream.Schema()
			if schema == nil {
				return 0, io.EOF
			}
			br.ipcWriter = ipc.NewWriter(&br.buf, ipc.WithSchema(schema))
			br.initWriter = true
		}

		if br.stream.Next() {
			rec := br.stream.Record()
			if rec != nil {
				if writeErr := br.ipcWriter.Write(rec); writeErr != nil {
					return 0, fmt.Errorf("failed to encode arrow batch: %w", writeErr)
				}
			}
		} else {
			if streamErr := br.stream.Err(); streamErr != nil {
				return 0, streamErr
			}
			if br.ipcWriter != nil {
				_ = br.ipcWriter.Close()
				br.ipcWriter = nil
			}
			br.closed = true
			if br.buf.Len() == 0 {
				return 0, io.EOF
			}
			break
		}
	}

	return br.buf.Read(p)
}

// Close closes the BatchReader and its underlying stream.
func (br *BatchReader) Close() error {
	br.closed = true
	if br.ipcWriter != nil {
		_ = br.ipcWriter.Close()
		br.ipcWriter = nil
	}
	return br.stream.Close()
}

// --- gRPC Interceptors ---

// AuthUnaryClientInterceptor returns a gRPC UnaryClientInterceptor that injects
// the Authorization: Bearer <token> header into request metadata.
func AuthUnaryClientInterceptor(token string) grpc.UnaryClientInterceptor {
	return func(ctx context.Context, method string, req, reply any, cc *grpc.ClientConn, invoker grpc.UnaryInvoker, opts ...grpc.CallOption) error {
		if token != "" {
			ctx = metadata.AppendToOutgoingContext(ctx, "authorization", "Bearer "+token)
		}
		return invoker(ctx, method, req, reply, cc, opts...)
	}
}

// AuthStreamClientInterceptor returns a gRPC StreamClientInterceptor that injects
// the Authorization: Bearer <token> header into request metadata.
func AuthStreamClientInterceptor(token string) grpc.StreamClientInterceptor {
	return func(ctx context.Context, desc *grpc.StreamDesc, cc *grpc.ClientConn, method string, streamer grpc.Streamer, opts ...grpc.CallOption) (grpc.ClientStream, error) {
		if token != "" {
			ctx = metadata.AppendToOutgoingContext(ctx, "authorization", "Bearer "+token)
		}
		return streamer(ctx, desc, cc, method, opts...)
	}
}

type metadataCarrier struct {
	md metadata.MD
}

func (c *metadataCarrier) Get(key string) string {
	vals := c.md.Get(key)
	if len(vals) > 0 {
		return vals[0]
	}
	return ""
}

func (c *metadataCarrier) Set(key, val string) {
	c.md.Set(key, val)
}

func (c *metadataCarrier) Keys() []string {
	keys := make([]string, 0, len(c.md))
	for k := range c.md {
		keys = append(keys, k)
	}
	return keys
}

func injectTrace(ctx context.Context) context.Context {
	md, ok := metadata.FromOutgoingContext(ctx)
	if !ok {
		md = metadata.New(nil)
	} else {
		md = md.Copy()
	}
	carrier := &metadataCarrier{md: md}
	otel.GetTextMapPropagator().Inject(ctx, carrier)
	return metadata.NewOutgoingContext(ctx, md)
}

// TraceUnaryClientInterceptor returns a gRPC UnaryClientInterceptor that propagates
// OpenTelemetry trace context via metadata.
func TraceUnaryClientInterceptor() grpc.UnaryClientInterceptor {
	return func(ctx context.Context, method string, req, reply any, cc *grpc.ClientConn, invoker grpc.UnaryInvoker, opts ...grpc.CallOption) error {
		ctx = injectTrace(ctx)
		return invoker(ctx, method, req, reply, cc, opts...)
	}
}

// TraceStreamClientInterceptor returns a gRPC StreamClientInterceptor that propagates
// OpenTelemetry trace context via metadata.
func TraceStreamClientInterceptor() grpc.StreamClientInterceptor {
	return func(ctx context.Context, desc *grpc.StreamDesc, cc *grpc.ClientConn, method string, streamer grpc.Streamer, opts ...grpc.CallOption) (grpc.ClientStream, error) {
		ctx = injectTrace(ctx)
		return streamer(ctx, desc, cc, method, opts...)
	}
}

// RetryUnaryClientInterceptor returns a gRPC UnaryClientInterceptor that performs exponential
// backoff retries when encountering codes.Unavailable.
func RetryUnaryClientInterceptor(maxRetries int, initialBackoff, maxBackoff time.Duration) grpc.UnaryClientInterceptor {
	return func(ctx context.Context, method string, req, reply any, cc *grpc.ClientConn, invoker grpc.UnaryInvoker, opts ...grpc.CallOption) error {
		var lastErr error
		backoff := initialBackoff
		for attempt := 0; attempt <= maxRetries; attempt++ {
			if err := ctx.Err(); err != nil {
				return err
			}
			err := invoker(ctx, method, req, reply, cc, opts...)
			if err == nil {
				return nil
			}
			lastErr = err
			if status.Code(err) == codes.Unavailable && attempt < maxRetries {
				select {
				case <-ctx.Done():
					return ctx.Err()
				case <-time.After(backoff):
					backoff = minDuration(backoff*2, maxBackoff)
					continue
				}
			}
			return err
		}
		return lastErr
	}
}

// RetryStreamClientInterceptor returns a gRPC StreamClientInterceptor that performs exponential
// backoff retries when encountering codes.Unavailable on stream establishment.
func RetryStreamClientInterceptor(maxRetries int, initialBackoff, maxBackoff time.Duration) grpc.StreamClientInterceptor {
	return func(ctx context.Context, desc *grpc.StreamDesc, cc *grpc.ClientConn, method string, streamer grpc.Streamer, opts ...grpc.CallOption) (grpc.ClientStream, error) {
		var lastErr error
		backoff := initialBackoff
		for attempt := 0; attempt <= maxRetries; attempt++ {
			if err := ctx.Err(); err != nil {
				return nil, err
			}
			cs, err := streamer(ctx, desc, cc, method, opts...)
			if err == nil {
				return cs, nil
			}
			lastErr = err
			if status.Code(err) == codes.Unavailable && attempt < maxRetries {
				select {
				case <-ctx.Done():
					return nil, ctx.Err()
				case <-time.After(backoff):
					backoff = minDuration(backoff*2, maxBackoff)
					continue
				}
			}
			return nil, err
		}
		return nil, lastErr
	}
}
