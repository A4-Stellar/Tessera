# Tessera Go SDK (`tessera-go`)

The official, idiomatic Go client SDK for [Tessera](https://github.com/A4-Stellar/Tessera), the real-world asset (RWA) tokenization and indexing platform on Stellar.

## Features

- **Full `context.Context` Support**: Strict cancellation and timeout propagation across all HTTP and WebSocket operations.
- **Strongly Typed Models**: Complete type safety for all Tessera REST API request/response payloads (Assets, Holders, Compliance, Dividends, Portfolio, Stats, Events).
- **Concurrent Event Listener Channels**: Real-time event streaming powered by WebSockets via Go channels (`<-chan Event`).
- **Resilient HTTP Engine**: Configurable exponential backoff with random jitter, rate-limit (`Retry-After` header) handling, and automatic idempotent retries.
- **Zero Third-Party Dependencies**: Pure Go standard-library implementation adhering to clean architecture principles.

---

## Architecture & Complexity Design

### Clean Architecture
The SDK is designed using decoupled layers:
- **`Client`**: Thread-safe facade encapsulating configuration, retry backoff, and HTTP connection pooling.
- **`Options`**: Extensible functional configuration (`WithBaseURL`, `WithTimeout`, `WithMaxRetries`, etc.).
- **`Endpoints`**: Pure methods taking `context.Context` and strongly typed parameters.
- **`EventSubscription`**: Asynchronous reader pump with non-blocking channel dispatch, keepalive ping/pong, and leak-free lifecycle management.

### Time & Space Complexity
- **REST Operations**: $O(1)$ client-side overhead; memory allocations are strictly bounded to the deserialization of the JSON payload.
- **WebSocket Event Streaming**: Non-blocking channel operations with bounded ring buffers ($O(1)$ time per event message, $O(M)$ buffer space where $M$ is channel capacity).
- **Concurrency Safety**: Full read-write mutex protection for dynamic configuration and atomic retry counters.

---

## Installation

```bash
go get github.com/A4-Stellar/Tessera/sdk/go
```

---

## Quick Start

### 1. Initialize Client

```go
package main

import (
	"context"
	"fmt"
	"log"
	"time"

	tessera "github.com/A4-Stellar/Tessera/sdk/go"
)

func main() {
	client, err := tessera.NewClient(
		tessera.WithBaseURL("https://api.tessera.example.com"),
		tessera.WithTimeout(15*time.Second),
		tessera.WithMaxRetries(3),
	)
	if err != nil {
		log.Fatalf("Failed to initialize Tessera client: %v", err)
	}

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	stats, err := client.GetStats(ctx)
	if err != nil {
		log.Fatalf("Failed to fetch stats: %v", err)
	}

	fmt.Printf("Total Assets: %d, TVL: $%.2f\n", stats.TotalAssets, stats.TvlUsd)
}
```

### 2. Portfolio Performance & Return Tracking

```go
ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
defer cancel()

portfolio, err := client.GetHolderPortfolio(ctx, "GDQOE23CFSUMSVQK4Y5JHPPMX73TKMGVUVCKEX6TV7WDZ5HQOFZ6AC5Z", nil)
if err != nil {
    log.Fatalf("Portfolio query failed: %v", err)
}

fmt.Printf("Holder Address: %s\n", portfolio.Address)
fmt.Printf("Current Valuation: $%.2f\n", portfolio.Summary.TotalValuationUsd)
fmt.Printf("Time-Weighted Return (TWR): %.2f%%\n", portfolio.Performance.TimeWeightedReturnPct)
fmt.Printf("Net Present Value (NPV): $%.2f\n", portfolio.Performance.NetPresentValue)
if portfolio.Performance.InternalRateOfReturnPct != nil {
    fmt.Printf("Internal Rate of Return (IRR): %.2f%%\n", *portfolio.Performance.InternalRateOfReturnPct)
}

// Inspect time-series data for charting
for _, pt := range portfolio.TimeSeries {
    fmt.Printf("[%s] Value: $%.2f, TWR: %.2f%%\n", pt.Timestamp, pt.PortfolioValueUsd, pt.CumulativeTwrPct)
}
```

### 3. Concurrent Event Listener via WebSockets

```go
ctx, cancel := context.WithCancel(context.Background())
defer cancel()

// Subscribe to specific topics (e.g. transfer, dividend)
sub, err := client.SubscribeEvents(ctx, "transfer", "dividend")
if err != nil {
    log.Fatalf("Failed to subscribe: %v", err)
}
defer sub.Close()

// Concurrent listener loop
for {
    select {
    case event, ok := <-sub.Events():
        if !ok {
            fmt.Println("Event stream closed")
            return
        }
        fmt.Printf("Received Event [%d] Type: %s on Ledger %d\n", event.ID, event.EventType, event.Ledger)
    case err := <-sub.Errors():
        log.Printf("Stream error: %v\n", err)
    case <-ctx.Done():
        fmt.Println("Context cancelled, exiting...")
        return
    }
}
```

---

## License

Licensed under the Apache License, Version 2.0.
