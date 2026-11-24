package tessera

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/apache/arrow-go/v18/arrow"
	"github.com/apache/arrow-go/v18/arrow/array"
	"github.com/apache/arrow-go/v18/arrow/flight"
	"github.com/apache/arrow-go/v18/arrow/ipc"
	"github.com/apache/arrow-go/v18/arrow/memory"
	"go.opentelemetry.io/otel"
	"go.opentelemetry.io/otel/propagation"
	"go.opentelemetry.io/otel/trace"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/metadata"
	"google.golang.org/grpc/status"
)

func TestClient_GetStats_Success(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/v1/stats" {
			t.Fatalf("expected path /v1/stats, got %s", r.URL.Path)
		}
		w.Header().Set("Content-Type", "application/json")
		_ = json.NewEncoder(w).Encode(Stats{
			TotalAssets:       5,
			ActiveAssets:      4,
			TvlCents:          "100000000",
			TvlUsd:            1000000.0,
			TotalHolders:      42,
			LastIndexedLedger: 12345,
		})
	}))
	defer ts.Close()

	client, err := NewClient(WithBaseURL(ts.URL))
	if err != nil {
		t.Fatalf("failed to create client: %v", err)
	}

	ctx := context.Background()
	stats, err := client.GetStats(ctx)
	if err != nil {
		t.Fatalf("GetStats failed: %v", err)
	}

	if stats.TotalAssets != 5 {
		t.Errorf("expected 5 assets, got %d", stats.TotalAssets)
	}
	if stats.TvlUsd != 1000000.0 {
		t.Errorf("expected TVL 1000000.0, got %f", stats.TvlUsd)
	}
}

func TestClient_ContextCancellation(t *testing.T) {
	blockServer := make(chan struct{})
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		<-blockServer
	}))
	defer ts.Close()
	defer close(blockServer)

	client, err := NewClient(WithBaseURL(ts.URL))
	if err != nil {
		t.Fatalf("failed to create client: %v", err)
	}

	ctx, cancel := context.WithCancel(context.Background())
	// Cancel after 50ms
	time.AfterFunc(50*time.Millisecond, cancel)

	start := time.Now()
	_, err = client.GetStats(ctx)
	elapsed := time.Since(start)

	if err == nil {
		t.Fatal("expected error due to cancelled context, got nil")
	}
	if elapsed > 1*time.Second {
		t.Errorf("request took too long to abort on cancellation: %s", elapsed)
	}
}

func TestClient_ContextTimeout(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		time.Sleep(200 * time.Millisecond)
		w.WriteHeader(http.StatusOK)
	}))
	defer ts.Close()

	client, err := NewClient(WithBaseURL(ts.URL))
	if err != nil {
		t.Fatalf("failed to create client: %v", err)
	}

	ctx, cancel := context.WithTimeout(context.Background(), 50*time.Millisecond)
	defer cancel()

	_, err = client.GetStats(ctx)
	if err == nil {
		t.Fatal("expected timeout error, got nil")
	}
}

func TestClient_RateLimit_Retry(t *testing.T) {
	var attempts int32
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		current := atomic.AddInt32(&attempts, 1)
		if current == 1 {
			w.Header().Set("Retry-After", "1")
			w.WriteHeader(http.StatusTooManyRequests)
			_, _ = w.Write([]byte(`{"error":"rate_limited","message":"slow down"}`))
			return
		}

		w.Header().Set("Content-Type", "application/json")
		_ = json.NewEncoder(w).Encode(Stats{TotalAssets: 10})
	}))
	defer ts.Close()

	client, err := NewClient(
		WithBaseURL(ts.URL),
		WithMaxRetries(2),
		WithBackoff(10*time.Millisecond, 50*time.Millisecond),
	)
	if err != nil {
		t.Fatalf("failed to create client: %v", err)
	}

	ctx := context.Background()
	stats, err := client.GetStats(ctx)
	if err != nil {
		t.Fatalf("request should succeed after retry, got: %v", err)
	}
	if stats.TotalAssets != 10 {
		t.Errorf("expected 10 assets, got %d", stats.TotalAssets)
	}
	if atomic.LoadInt32(&attempts) != 2 {
		t.Errorf("expected exactly 2 attempts, got %d", attempts)
	}
}

func TestClient_RetryReplaysRequestBody(t *testing.T) {
	var attempts int32
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		body, err := io.ReadAll(r.Body)
		if err != nil {
			t.Fatalf("read request body: %v", err)
		}
		if string(body) != `{"actor":"admin"}` {
			t.Errorf("unexpected request body %q", body)
		}
		if atomic.AddInt32(&attempts, 1) == 1 {
			w.WriteHeader(http.StatusTooManyRequests)
			return
		}
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write([]byte(`{}`))
	}))
	defer ts.Close()

	client, err := NewClient(WithBaseURL(ts.URL), WithMaxRetries(1), WithBackoff(time.Millisecond, time.Millisecond))
	if err != nil {
		t.Fatal(err)
	}
	_, err = client.executeRequest(context.Background(), http.MethodPost, "/v1/audit/entries", nil,
		strings.NewReader(`{"actor":"admin"}`))
	if err != nil {
		t.Fatalf("request failed after retry: %v", err)
	}
	if got := atomic.LoadInt32(&attempts); got != 2 {
		t.Fatalf("expected two attempts, got %d", got)
	}
}

func TestClient_GetHolderPortfolio_Success(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/v1/holders/GADDR123/portfolio" {
			t.Fatalf("unexpected path: %s", r.URL.Path)
		}
		w.Header().Set("Content-Type", "application/json")
		_ = json.NewEncoder(w).Encode(HolderPortfolioResponse{
			Address: "GADDR123",
			Summary: PortfolioSummary{
				TotalValuationUsd:      50000.0,
				TotalAssetsHeld:        2,
				TotalCashInflowsUsd:    40000.0,
				NetInvestedCapitalUsd:  40000.0,
				TotalUnrealizedGainUsd: 10000.0,
			},
			Holdings: []PortfolioHoldingDetail{
				{
					AssetID:        1,
					AssetName:      "Real Estate Fund",
					Symbol:         "REF",
					MarketValueUsd: 50000.0,
				},
			},
			Performance: PortfolioPerformance{
				TimeWeightedReturn:    0.25,
				TimeWeightedReturnPct: 25.0,
				NetPresentValue:       7500.0,
				DiscountRate:          0.05,
			},
			TimeSeries: []PortfolioTimeSeriesPoint{
				{
					Timestamp:         "2024-01-01T00:00:00Z",
					PortfolioValueUsd: 50000.0,
					CumulativeTwrPct:  25.0,
				},
			},
		})
	}))
	defer ts.Close()

	client, err := NewClient(WithBaseURL(ts.URL))
	if err != nil {
		t.Fatalf("failed to create client: %v", err)
	}

	portfolio, err := client.GetHolderPortfolio(context.Background(), "GADDR123", nil)
	if err != nil {
		t.Fatalf("GetHolderPortfolio failed: %v", err)
	}

	if portfolio.Address != "GADDR123" {
		t.Errorf("expected GADDR123, got %s", portfolio.Address)
	}
	if portfolio.Summary.TotalValuationUsd != 50000.0 {
		t.Errorf("expected valuation 50000.0, got %f", portfolio.Summary.TotalValuationUsd)
	}
	if portfolio.Performance.TimeWeightedReturnPct != 25.0 {
		t.Errorf("expected TWR 25%%, got %f", portfolio.Performance.TimeWeightedReturnPct)
	}
	if len(portfolio.TimeSeries) != 1 {
		t.Errorf("expected 1 timeseries point, got %d", len(portfolio.TimeSeries))
	}
}

func TestClient_ConcurrentCalls(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		_ = json.NewEncoder(w).Encode(Stats{TotalAssets: 1})
	}))
	defer ts.Close()

	client, err := NewClient(WithBaseURL(ts.URL))
	if err != nil {
		t.Fatalf("failed to create client: %v", err)
	}

	const goroutines = 20
	var wg sync.WaitGroup
	wg.Add(goroutines)

	for i := 0; i < goroutines; i++ {
		go func() {
			defer wg.Done()
			ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
			defer cancel()
			_, err := client.GetStats(ctx)
			if err != nil {
				t.Errorf("concurrent call failed: %v", err)
			}
		}()
	}

	wg.Wait()
}

func TestClient_WebSocket_MockServer(t *testing.T) {
	// Create a TCP listener for a mock WebSocket server
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("failed to create tcp listener: %v", err)
	}
	defer listener.Close()

	port := listener.Addr().(*net.TCPAddr).Port
	wsURL := fmt.Sprintf("ws://127.0.0.1:%d/v1/ws", port)
	doneChan := make(chan struct{})
	defer close(doneChan)

	// Mock WebSocket server handling handshake and sending one event
	go func() {
		conn, err := listener.Accept()
		if err != nil {
			return
		}
		defer conn.Close()

		// Read HTTP handshake request
		buf := make([]byte, 1024)
		n, _ := conn.Read(buf)
		reqStr := string(buf[:n])

		// Extract Sec-WebSocket-Key
		var clientKey string
		for _, line := range splitLines(reqStr) {
			if startsWith(line, "Sec-WebSocket-Key:") {
				clientKey = trimSpace(line[len("Sec-WebSocket-Key:"):])
			}
		}

		acceptKey := computeWebSocketAccept(clientKey)
		resp := fmt.Sprintf(
			"HTTP/1.1 101 Switching Protocols\r\n"+
				"Upgrade: websocket\r\n"+
				"Connection: Upgrade\r\n"+
				"Sec-WebSocket-Accept: %s\r\n\r\n",
			acceptKey,
		)
		_, _ = conn.Write([]byte(resp))

		// Ensure client finishes reading handshake response before writing the frame
		time.Sleep(50 * time.Millisecond)

		// Send an Event frame: opcode 0x81 (FIN + Text)
		eventPayload := []byte(`{"id":100,"contract":"C123","event_type":"Transfer","ledger":42,"data":{"amount":"500"}}`)
		frame := []byte{0x81, byte(len(eventPayload))}
		frame = append(frame, eventPayload...)
		_, _ = conn.Write(frame)

		// Wait until test context is done before closing
		<-doneChan
	}()

	client, err := NewClient(WithWebSocketURL(wsURL))
	if err != nil {
		t.Fatalf("failed to create client: %v", err)
	}

	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()

	sub, err := client.SubscribeEvents(ctx, "transfer")
	if err != nil {
		t.Fatalf("SubscribeEvents failed: %v", err)
	}
	defer sub.Close()

	select {
	case ev, ok := <-sub.Events():
		if !ok {
			t.Fatal("events channel closed prematurely")
		}
		if ev.ID != 100 {
			t.Errorf("expected event ID 100, got %d", ev.ID)
		}
		if ev.EventType != "Transfer" {
			t.Errorf("expected Transfer, got %s", ev.EventType)
		}
	case err := <-sub.Errors():
		t.Fatalf("received unexpected error: %v", err)
	case <-time.After(1 * time.Second):
		t.Fatal("timed out waiting for event")
	}
}

func splitLines(s string) []string {
	var lines []string
	start := 0
	for i := 0; i < len(s); i++ {
		if s[i] == '\n' {
			line := s[start:i]
			if len(line) > 0 && line[len(line)-1] == '\r' {
				line = line[:len(line)-1]
			}
			lines = append(lines, line)
			start = i + 1
		}
	}
	if start < len(s) {
		lines = append(lines, s[start:])
	}
	return lines
}

func startsWith(s, prefix string) bool {
	return len(s) >= len(prefix) && s[:len(prefix)] == prefix
}

func trimSpace(s string) string {
	start := 0
	for start < len(s) && (s[start] == ' ' || s[start] == '\t' || s[start] == '\r' || s[start] == '\n') {
		start++
	}
	end := len(s)
	for end > start && (s[end-1] == ' ' || s[end-1] == '\t' || s[end-1] == '\r' || s[end-1] == '\n') {
		end--
	}
	return s[start:end]
}

// MockFlightServer provides an in-memory gRPC Arrow Flight server for unit tests.
type MockFlightServer struct {
	flight.BaseFlightServer
	schema           *arrow.Schema
	records          []arrow.Record
	unavailableCount atomic.Int32
	receivedAuth     atomic.Value
	receivedTrace    atomic.Value
	lastTicket       atomic.Value
}

func (s *MockFlightServer) DoGet(ticket *flight.Ticket, stream flight.FlightService_DoGetServer) error {
	md, ok := metadata.FromIncomingContext(stream.Context())
	if ok {
		auths := md.Get("authorization")
		if len(auths) > 0 {
			s.receivedAuth.Store(auths[0])
		}
		traces := md.Get("traceparent")
		if len(traces) > 0 {
			s.receivedTrace.Store(traces[0])
		}
	}
	s.lastTicket.Store(string(ticket.Ticket))

	if n := s.unavailableCount.Load(); n > 0 {
		s.unavailableCount.Add(-1)
		return status.Error(codes.Unavailable, "server temporarily unavailable")
	}

	wtr := flight.NewRecordWriter(stream, ipc.WithSchema(s.schema))
	defer wtr.Close()

	for _, rec := range s.records {
		if err := wtr.Write(rec); err != nil {
			return err
		}
	}
	return nil
}

func startMockFlightServer(t *testing.T, srv *MockFlightServer) (string, func()) {
	lis, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("failed to listen: %v", err)
	}
	grpcServer := grpc.NewServer()
	flight.RegisterFlightServiceServer(grpcServer, srv)
	go func() {
		_ = grpcServer.Serve(lis)
	}()
	cleanup := func() {
		grpcServer.Stop()
		_ = lis.Close()
	}
	return lis.Addr().String(), cleanup
}

func createTestSchemaAndRecords(t *testing.T) (*arrow.Schema, []arrow.Record) {
	schema := arrow.NewSchema([]arrow.Field{
		{Name: "asset_id", Type: arrow.PrimitiveTypes.Uint64},
		{Name: "asset_name", Type: arrow.BinaryTypes.String},
		{Name: "symbol", Type: arrow.BinaryTypes.String},
		{Name: "ledger", Type: arrow.PrimitiveTypes.Uint32},
		{Name: "address", Type: arrow.BinaryTypes.String},
		{Name: "balance", Type: arrow.BinaryTypes.String},
		{Name: "share_percent", Type: arrow.PrimitiveTypes.Float64},
	}, nil)

	var records []arrow.Record
	for i := 0; i < 3; i++ {
		b := array.NewRecordBuilder(memory.DefaultAllocator, schema)
		b.Field(0).(*array.Uint64Builder).Append(uint64(i + 1))
		b.Field(1).(*array.StringBuilder).Append(fmt.Sprintf("Asset %d", i+1))
		b.Field(2).(*array.StringBuilder).Append(fmt.Sprintf("AST%d", i+1))
		b.Field(3).(*array.Uint32Builder).Append(uint32(1000 + i))
		b.Field(4).(*array.StringBuilder).Append(fmt.Sprintf("GADDRESS%d", i+1))
		b.Field(5).(*array.StringBuilder).Append("5000000000")
		b.Field(6).(*array.Float64Builder).Append(33.33)
		rec := b.NewRecord()
		records = append(records, rec)
		b.Release()
	}

	return schema, records
}

func TestFlightClient_DoGet_Streaming(t *testing.T) {
	schema, records := createTestSchemaAndRecords(t)
	defer func() {
		for _, r := range records {
			r.Release()
		}
	}()

	mockSrv := &MockFlightServer{
		schema:  schema,
		records: records,
	}
	addr, cleanup := startMockFlightServer(t, mockSrv)
	defer cleanup()

	fc, err := NewFlightClient(addr)
	if err != nil {
		t.Fatalf("failed to create flight client: %v", err)
	}
	defer fc.Close()

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	stream, err := fc.ExportAssetArrow(ctx, 42)
	if err != nil {
		t.Fatalf("ExportAssetArrow failed: %v", err)
	}
	defer stream.Close()

	if string(stream.Schema().Field(0).Name) != "asset_id" {
		t.Errorf("unexpected schema field: %s", stream.Schema().Field(0).Name)
	}

	count := 0
	for stream.Next() {
		rec := stream.Record()
		if rec == nil {
			t.Fatal("expected non-nil record")
		}
		if rec.NumRows() != 1 {
			t.Errorf("expected 1 row per record, got %d", rec.NumRows())
		}
		count++
	}

	if err := stream.Err(); err != nil {
		t.Fatalf("stream encountered error: %v", err)
	}
	if count != 3 {
		t.Errorf("expected 3 record batches, got %d", count)
	}
	if mockSrv.lastTicket.Load() != "/v1/assets/42/export?format=arrow" {
		t.Errorf("expected ticket /v1/assets/42/export?format=arrow, got %v", mockSrv.lastTicket.Load())
	}
}

func TestFlightClient_BatchReader_IOReader(t *testing.T) {
	schema, records := createTestSchemaAndRecords(t)
	defer func() {
		for _, r := range records {
			r.Release()
		}
	}()

	mockSrv := &MockFlightServer{
		schema:  schema,
		records: records,
	}
	addr, cleanup := startMockFlightServer(t, mockSrv)
	defer cleanup()

	fc, err := NewFlightClient(addr)
	if err != nil {
		t.Fatalf("failed to create flight client: %v", err)
	}
	defer fc.Close()

	ctx := context.Background()
	stream, err := fc.ExportAssetArrow(ctx, 100)
	if err != nil {
		t.Fatalf("ExportAssetArrow failed: %v", err)
	}
	defer stream.Close()

	reader := stream.NewBatchReader()
	defer reader.Close()

	data, err := io.ReadAll(reader)
	if err != nil {
		t.Fatalf("io.ReadAll on BatchReader failed: %v", err)
	}
	if len(data) == 0 {
		t.Fatal("expected non-empty data from BatchReader")
	}

	// Verify the streamed bytes can be read back as valid Arrow IPC record batches
	ipcReader, err := ipc.NewReader(bytes.NewReader(data))
	if err != nil {
		t.Fatalf("failed to initialize IPC reader: %v", err)
	}
	defer ipcReader.Release()

	readBatches := 0
	for ipcReader.Next() {
		rec := ipcReader.Record()
		if rec.NumRows() != 1 {
			t.Errorf("expected 1 row, got %d", rec.NumRows())
		}
		readBatches++
	}
	if readBatches != 3 {
		t.Errorf("expected 3 batches read back, got %d", readBatches)
	}
}

func TestFlightClient_Interceptors_AuthAndTrace(t *testing.T) {
	schema, records := createTestSchemaAndRecords(t)
	defer func() {
		for _, r := range records {
			r.Release()
		}
	}()

	mockSrv := &MockFlightServer{
		schema:  schema,
		records: records,
	}
	addr, cleanup := startMockFlightServer(t, mockSrv)
	defer cleanup()

	// Setup OpenTelemetry propagator
	otel.SetTextMapPropagator(propagation.TraceContext{})

	token := "secret-jwt-flight-token"
	fc, err := NewFlightClient(addr, WithFlightAuthToken(token))
	if err != nil {
		t.Fatalf("failed to create flight client: %v", err)
	}
	defer fc.Close()

	// Create trace context
	traceID, _ := trace.TraceIDFromHex("4bf92f3577b34da6a3ce929d0e0e4736")
	spanID, _ := trace.SpanIDFromHex("00f067aa0ba902b7")
	spanCtx := trace.NewSpanContext(trace.SpanContextConfig{
		TraceID:    traceID,
		SpanID:     spanID,
		TraceFlags: trace.FlagsSampled,
	})
	ctx := trace.ContextWithSpanContext(context.Background(), spanCtx)

	stream, err := fc.ExportAssetArrow(ctx, 1)
	if err != nil {
		t.Fatalf("ExportAssetArrow failed: %v", err)
	}
	_ = stream.Close()

	gotAuth, _ := mockSrv.receivedAuth.Load().(string)
	if gotAuth != "Bearer "+token {
		t.Errorf("expected auth header Bearer %s, got %q", token, gotAuth)
	}

	gotTrace, _ := mockSrv.receivedTrace.Load().(string)
	if !strings.Contains(gotTrace, "4bf92f3577b34da6a3ce929d0e0e4736") {
		t.Errorf("expected traceparent with trace ID, got %q", gotTrace)
	}
}

func TestFlightClient_RetryOnUnavailable(t *testing.T) {
	schema, records := createTestSchemaAndRecords(t)
	defer func() {
		for _, r := range records {
			r.Release()
		}
	}()

	mockSrv := &MockFlightServer{
		schema:  schema,
		records: records,
	}
	// Fail the first 2 calls with codes.Unavailable
	mockSrv.unavailableCount.Store(2)

	addr, cleanup := startMockFlightServer(t, mockSrv)
	defer cleanup()

	fc, err := NewFlightClient(
		addr,
		WithFlightRetry(3, 10*time.Millisecond, 50*time.Millisecond),
	)
	if err != nil {
		t.Fatalf("failed to create flight client: %v", err)
	}
	defer fc.Close()

	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()

	stream, err := fc.ExportAssetArrow(ctx, 1)
	if err != nil {
		t.Fatalf("ExportAssetArrow should have succeeded after retry, got: %v", err)
	}
	defer stream.Close()

	if !stream.Next() {
		t.Fatal("expected batch after retrying unavailable")
	}
}
