// Package tessera provides the official Go software development kit (SDK) for Tessera,
// an institutional-grade platform for real-world asset (RWA) tokenization on Stellar and Soroban.
//
// The SDK offers comprehensive support for:
//   - REST API interactions with automated exponential backoff retries, jitter, and rate limiting (HTTP 429 Retry-After).
//   - Real-time WebSocket event streaming with RFC 6455 framing and topic filtering.
//   - Apache Arrow Flight gRPC streaming for high-throughput columnar asset data export.
//   - Direct Parquet writer integration via streaming BatchReader satisfying io.Reader.
//   - gRPC interceptors for bearer authentication, W3C OpenTelemetry trace propagation, and retry on UNAVAILABLE.
//
// # Getting Started
//
// Construct a REST client with NewClient:
//
//	client, err := tessera.NewClient(
//	    tessera.WithBaseURL("https://api.tessera.network"),
//	    tessera.WithAuthToken("your-jwt-token"),
//	)
//	if err != nil {
//	    log.Fatalf("failed to create client: %v", err)
//	}
//
//	stats, err := client.GetStats(ctx)
//	if err != nil {
//	    log.Fatalf("failed to get stats: %v", err)
//	}
//	fmt.Printf("Total assets: %d, TVL USD: %.2f\n", stats.TotalAssets, stats.TvlUsd)
//
// # Arrow Flight gRPC Streaming
//
// For large cap-table or ledger exports, use FlightClient:
//
//	fc, err := client.FlightClient("flight.tessera.network:443")
//	if err != nil {
//	    log.Fatalf("failed to connect to flight service: %v", err)
//	}
//	defer fc.Close()
//
//	stream, err := fc.ExportAssetArrow(ctx, 123)
//	if err != nil {
//	    log.Fatalf("export failed: %v", err)
//	}
//	defer stream.Close()
//
//	for stream.Next() {
//	    batch := stream.Record()
//	    fmt.Printf("Streamed batch with %d rows\n", batch.NumRows())
//	}
//
// # Streaming directly to Parquet
//
// Convert the Flight stream into an io.Reader without loading the entire response into memory:
//
//	batchReader := stream.NewBatchReader()
//	defer batchReader.Close()
//
//	outFile, err := os.Create("export.parquet")
//	if err != nil {
//	    log.Fatal(err)
//	}
//	defer outFile.Close()
//
//	_, err = io.Copy(outFile, batchReader)
//
// # Godoc & Publishing
//
// This package is published to pkg.go.dev at https://pkg.go.dev/github.com/A4-Stellar/Tessera/sdk/go.
package tessera
