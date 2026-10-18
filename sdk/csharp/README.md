# Tessera .NET SDK (`Tessera.SDK`)

The official C# client for [Tessera](https://github.com/A4-Stellar/Tessera), the real-world asset (RWA) tokenization and indexing platform on Stellar.

- **Typed REST client** for every Tessera endpoint (`/v1/stats`, assets, holders, compliance, dividends, analytics, portfolio, events, security, audit, MMR proofs, health/version/metrics).
- **`IHttpClientFactory` integration**: `services.AddTesseraClient(...)` registers `ITesseraClient` as a typed client with a pooled `HttpClient`.
- **Event streaming with `IAsyncEnumerable<TesseraEvent>`** over the `/v1/ws` WebSocket.
- **Resilient transport**: exponential backoff with jitter, `Retry-After` support for HTTP 429, retries only for idempotent requests on network and 5xx failures.
- **Typed errors**: `TesseraApiException`, `TesseraRateLimitException`, `TesseraNetworkException`, `TesseraSerializationException`.
- Targets **.NET 8**; the only dependency is `Microsoft.Extensions.Http`.

## Install

```bash
dotnet add package Tessera.SDK
```

## Quick start (ASP.NET Core / generic host)

```csharp
using Tessera.SDK;

builder.Services
    .AddTesseraClient(options =>
    {
        options.BaseAddress = new Uri("https://api.tessera.example.com");
        options.AuthToken = builder.Configuration["Tessera:Token"]; // optional
        options.Timeout = TimeSpan.FromSeconds(15);
        options.MaxRetries = 3;
    });
// The returned IHttpClientBuilder accepts extra handlers, e.g. .AddHttpMessageHandler<AuditHandler>()

public sealed class TreasuryService(ITesseraClient tessera)
{
    public async Task<double> TotalValueLockedAsync(CancellationToken ct)
    {
        var stats = await tessera.GetStatsAsync(ct);
        return stats.TvlUsd;
    }
}
```

## Without dependency injection

```csharp
using var client = TesseraClient.Create(new TesseraClientOptions
{
    BaseAddress = new Uri("https://api.tessera.example.com"),
});

var assets = await client.ListAssetsAsync(new ListAssetsQuery { AssetType = "real_estate", Active = true, Limit = 20 });
foreach (var asset in assets)
{
    var holders = await client.GetAssetHoldersAsync(asset.Id, new PageQuery { Limit = 10 });
    Console.WriteLine($"{asset.Symbol}: {holders.Count} top holders");
}
```

`TesseraClient.Create(options)` owns a `SocketsHttpHandler` with a bounded connection lifetime; `TesseraClient.Create(httpClient, options)` wraps an `HttpClient` you manage.

## Streaming events

```csharp
using var cts = new CancellationTokenSource(TimeSpan.FromMinutes(10));

await foreach (var evt in client.StreamEventsAsync(new[] { "transfer", "dividend" }, cts.Token))
{
    Console.WriteLine($"{evt.Ledger} {evt.EventType} {evt.Contract} {evt.Data}");
}
```

The client opens `/v1/ws` (derived from `BaseAddress` — `https` becomes `wss` — or set `WebSocketAddress`), sends one `subscribe:<topic>` frame per topic, and yields each event the server pushes. A topic matches an event when it appears, case-insensitively, in the event type, contract ID or payload. Breaking out of the loop or cancelling the token closes the socket.

## Errors

| Exception | When |
| --- | --- |
| `TesseraApiException` | Non-2xx response. `StatusCode`, `ErrorCode` (e.g. `not_found`) and `ApiMessage` come from the API's `{ "error", "message" }` body. |
| `TesseraRateLimitException` | HTTP 429 after retries; `RetryAfter` carries the server's hint. |
| `TesseraNetworkException` | DNS, connection, TLS or timeout failure after retries. |
| `TesseraSerializationException` | A body or event frame did not match the expected model. |

Caller cancellation surfaces as `OperationCanceledException` and is never retried.

Token amounts and cent values (`TotalSupply`, `Balance`, `ValuationCents`, …) are strings to preserve the API's `i128` precision; parse them with `BigInteger.Parse` when you need arithmetic.

## Build, test and pack

```bash
cd sdk/csharp
dotnet test -c Release
dotnet pack src/Tessera.SDK -c Release -o artifacts   # Tessera.SDK.<version>.nupkg + .snupkg
dotnet nuget push artifacts/Tessera.SDK.*.nupkg --source https://api.nuget.org/v3/index.json --api-key "$NUGET_API_KEY"
```

The package version is set in `src/Tessera.SDK/Tessera.SDK.csproj` (`<Version>`) and can be overridden at pack time with `-p:Version=1.2.3`.
