package tessera

import (
	"context"
	"encoding/json"
	"fmt"
	"net"
	"net/http"
	"net/http/httptest"
	"sync"
	"sync/atomic"
	"testing"
	"time"
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

func TestClient_GetHolderPortfolio_Success(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/v1/holders/GADDR123/portfolio" {
			t.Fatalf("unexpected path: %s", r.URL.Path)
		}
		w.Header().Set("Content-Type", "application/json")
		_ = json.NewEncoder(w).Encode(HolderPortfolioResponse{
			Address: "GADDR123",
			Summary: PortfolioSummary{
				TotalValuationUsd:     50000.0,
				TotalAssetsHeld:       2,
				TotalCashInflowsUsd:   40000.0,
				NetInvestedCapitalUsd: 40000.0,
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

		// Send an Event frame: opcode 0x81 (FIN + Text)
		eventPayload := []byte(`{"id":100,"contract":"C123","event_type":"Transfer","ledger":42,"data":{"amount":"500"}}`)
		frame := []byte{0x81, byte(len(eventPayload))}
		frame = append(frame, eventPayload...)
		_, _ = conn.Write(frame)

		// Wait briefly then close
		time.Sleep(100 * time.Millisecond)
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
