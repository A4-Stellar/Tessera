package tessera

import (
	"bufio"
	"context"
	"crypto/rand"
	"crypto/sha1"
	"crypto/tls"
	"encoding/base64"
	"encoding/binary"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/url"
	"strings"
	"sync"
)

// EventSubscription manages an active WebSocket connection listening for Tessera events.
type EventSubscription struct {
	conn       net.Conn
	events     chan Event
	errors     chan error
	closeOnce  sync.Once
	closed     chan struct{}
	cancelFunc context.CancelFunc
}

// Events returns the receive-only channel for streaming contract events.
func (s *EventSubscription) Events() <-chan Event {
	return s.events
}

// Errors returns the receive-only channel for connection errors.
func (s *EventSubscription) Errors() <-chan error {
	return s.errors
}

// Close terminates the subscription and releases network resources.
func (s *EventSubscription) Close() error {
	var err error
	s.closeOnce.Do(func() {
		close(s.closed)
		if s.cancelFunc != nil {
			s.cancelFunc()
		}
		if s.conn != nil {
			// Best-effort WebSocket close frame
			_ = sendCloseFrame(s.conn)
			err = s.conn.Close()
		}
	})
	return err
}

// SubscribeEvents establishes a WebSocket connection to Tessera's real-time event stream
// and subscribes to the provided topics. Returns read-only channels for events and errors.
//
// The connection and background reader goroutine automatically terminate when ctx is cancelled.
func (c *Client) SubscribeEvents(ctx context.Context, topics ...string) (*EventSubscription, error) {
	wsURL := c.config.WebSocketURL
	if wsURL == "" {
		// Infer from BaseURL
		u, err := url.Parse(c.config.BaseURL)
		if err != nil {
			return nil, fmt.Errorf("invalid base URL for WebSocket: %w", err)
		}
		scheme := "ws"
		if u.Scheme == "https" {
			scheme = "wss"
		}
		wsURL = fmt.Sprintf("%s://%s/v1/ws", scheme, u.Host)
	}

	u, err := url.Parse(wsURL)
	if err != nil {
		return nil, fmt.Errorf("invalid websocket URL %q: %w", wsURL, err)
	}

	// Dial TCP or TLS
	conn, err := dialWebSocket(ctx, u)
	if err != nil {
		return nil, &NetworkError{Op: "dial_ws", Err: err}
	}

	// Perform RFC 6455 WebSocket Handshake
	if err := performWebSocketHandshake(conn, u, c.config.AuthToken); err != nil {
		_ = conn.Close()
		return nil, &NetworkError{Op: "ws_handshake", Err: err}
	}

	subCtx, cancel := context.WithCancel(ctx)
	sub := &EventSubscription{
		conn:       conn,
		events:     make(chan Event, 256),
		errors:     make(chan error, 16),
		closed:     make(chan struct{}),
		cancelFunc: cancel,
	}

	// Send subscription messages
	for _, topic := range topics {
		msg := fmt.Sprintf("subscribe:%s", strings.TrimSpace(topic))
		if err := sendTextFrame(conn, msg); err != nil {
			_ = sub.Close()
			return nil, fmt.Errorf("failed to send subscription for topic %q: %w", topic, err)
		}
	}

	// Launch concurrent event listener pump
	go sub.readPump(subCtx)

	return sub, nil
}

func (s *EventSubscription) readPump(ctx context.Context) {
	defer func() {
		_ = s.Close()
		close(s.events)
		close(s.errors)
	}()

	reader := bufio.NewReader(s.conn)

	// Watch for context cancellation in a background goroutine
	go func() {
		select {
		case <-ctx.Done():
			_ = s.Close()
		case <-s.closed:
		}
	}()

	for {
		select {
		case <-ctx.Done():
			return
		case <-s.closed:
			return
		default:
		}

		opcode, payload, err := readFrame(reader)
		if err != nil {
			if !errors.Is(err, net.ErrClosed) && !isClosedError(err) {
				select {
				case s.errors <- err:
				default:
				}
			}
			return
		}

		switch opcode {
		case 0x1: // Text frame
			var event Event
			if err := json.Unmarshal(payload, &event); err == nil {
				select {
				case s.events <- event:
				case <-ctx.Done():
					return
				}
			} else {
				select {
				case s.errors <- fmt.Errorf("failed to decode event frame: %w", err):
				default:
				}
			}
		case 0x8: // Close frame
			return
		case 0x9: // Ping frame
			_ = sendFrame(s.conn, 0xA, payload) // Reply with Pong
		case 0xA: // Pong frame
			// Keepalive acknowledged
		}
	}
}

func isClosedError(err error) bool {
	if err == nil {
		return false
	}
	s := err.Error()
	return strings.Contains(s, "use of closed network connection") || strings.Contains(s, "EOF")
}

// Low-level RFC 6455 implementation with zero third-party dependencies.

func dialWebSocket(ctx context.Context, u *url.URL) (net.Conn, error) {
	host := u.Host
	if !strings.Contains(host, ":") {
		if u.Scheme == "wss" {
			host += ":443"
		} else {
			host += ":80"
		}
	}

	var d net.Dialer
	conn, err := d.DialContext(ctx, "tcp", host)
	if err != nil {
		return nil, err
	}

	if u.Scheme == "wss" {
		serverName := u.Hostname()
		tlsConn := tls.Client(conn, &tls.Config{
			ServerName: serverName,
		})
		if err := tlsConn.HandshakeContext(ctx); err != nil {
			_ = conn.Close()
			return nil, err
		}
		return tlsConn, nil
	}

	return conn, nil
}

func performWebSocketHandshake(conn net.Conn, u *url.URL, authToken string) error {
	nonce := make([]byte, 16)
	if _, err := rand.Read(nonce); err != nil {
		return err
	}
	key := base64.StdEncoding.EncodeToString(nonce)

	path := u.Path
	if path == "" {
		path = "/v1/ws"
	}
	if u.RawQuery != "" {
		path = fmt.Sprintf("%s?%s", path, u.RawQuery)
	}

	req := fmt.Sprintf(
		"GET %s HTTP/1.1\r\n"+
			"Host: %s\r\n"+
			"Upgrade: websocket\r\n"+
			"Connection: Upgrade\r\n"+
			"Sec-WebSocket-Key: %s\r\n"+
			"Sec-WebSocket-Version: 13\r\n",
		path, u.Host, key,
	)

	if authToken != "" {
		req += fmt.Sprintf("Authorization: Bearer %s\r\n", authToken)
	}
	req += "\r\n"

	if _, err := conn.Write([]byte(req)); err != nil {
		return err
	}

	reader := bufio.NewReader(conn)
	resp, err := http.ReadResponse(reader, &http.Request{Method: "GET"})
	if err != nil {
		return err
	}

	if resp.StatusCode != http.StatusSwitchingProtocols {
		return fmt.Errorf("unexpected status code during websocket upgrade: %d", resp.StatusCode)
	}

	expectedAccept := computeWebSocketAccept(key)
	if resp.Header.Get("Sec-WebSocket-Accept") != expectedAccept {
		return fmt.Errorf("invalid Sec-WebSocket-Accept response header")
	}

	return nil
}

func computeWebSocketAccept(key string) string {
	const magic = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
	h := sha1.New()
	h.Write([]byte(key + magic))
	return base64.StdEncoding.EncodeToString(h.Sum(nil))
}

func sendTextFrame(conn net.Conn, text string) error {
	return sendFrame(conn, 0x1, []byte(text))
}

func sendCloseFrame(conn net.Conn) error {
	return sendFrame(conn, 0x8, nil)
}

func sendFrame(conn net.Conn, opcode byte, payload []byte) error {
	var header []byte
	length := len(payload)

	// FIN bit set (0x80) + opcode
	firstByte := 0x80 | (opcode & 0x0F)
	header = append(header, firstByte)

	// Client-to-server frames MUST be masked (0x80 bit set)
	if length <= 125 {
		header = append(header, 0x80|byte(length))
	} else if length <= 65535 {
		header = append(header, 0x80|126)
		lenBytes := make([]byte, 2)
		binary.BigEndian.PutUint16(lenBytes, uint16(length))
		header = append(header, lenBytes...)
	} else {
		header = append(header, 0x80|127)
		lenBytes := make([]byte, 8)
		binary.BigEndian.PutUint64(lenBytes, uint64(length))
		header = append(header, lenBytes...)
	}

	// 4-byte random masking key
	mask := make([]byte, 4)
	if _, err := rand.Read(mask); err != nil {
		return err
	}
	header = append(header, mask...)

	maskedPayload := make([]byte, length)
	for i := 0; i < length; i++ {
		maskedPayload[i] = payload[i] ^ mask[i%4]
	}

	if _, err := conn.Write(header); err != nil {
		return err
	}
	if length > 0 {
		if _, err := conn.Write(maskedPayload); err != nil {
			return err
		}
	}
	return nil
}

func readFrame(r *bufio.Reader) (byte, []byte, error) {
	b0, err := r.ReadByte()
	if err != nil {
		return 0, nil, err
	}
	opcode := b0 & 0x0F

	b1, err := r.ReadByte()
	if err != nil {
		return 0, nil, err
	}
	isMasked := (b1 & 0x80) != 0
	payloadLen := int(b1 & 0x7F)

	if payloadLen == 126 {
		var l uint16
		if err := binary.Read(r, binary.BigEndian, &l); err != nil {
			return 0, nil, err
		}
		payloadLen = int(l)
	} else if payloadLen == 127 {
		var l uint64
		if err := binary.Read(r, binary.BigEndian, &l); err != nil {
			return 0, nil, err
		}
		payloadLen = int(l)
	}

	var mask [4]byte
	if isMasked {
		if _, err := io.ReadFull(r, mask[:]); err != nil {
			return 0, nil, err
		}
	}

	payload := make([]byte, payloadLen)
	if _, err := io.ReadFull(r, payload); err != nil {
		return 0, nil, err
	}

	if isMasked {
		for i := 0; i < payloadLen; i++ {
			payload[i] ^= mask[i%4]
		}
	}

	return opcode, payload, nil
}
