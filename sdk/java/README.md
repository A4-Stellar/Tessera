# Tessera Java SDK (`tessera-java-sdk`)

The official JVM client for [Tessera](https://github.com/A4-Stellar/Tessera), the real-world asset (RWA) tokenization and indexing platform on Stellar. It is written in Java 17 and is equally usable from Kotlin.

- **Jackson-annotated models** (`io.tessera.sdk.model`) for every REST payload: assets, holders, compliance, dividends, analytics, portfolio, events, security anomalies, audit log and anchors.
- **Synchronous and asynchronous APIs**: every endpoint has a blocking method (`getStats()`) and a `CompletableFuture` method (`getStatsAsync()`).
- **WebSocket event streaming** with live `subscribe` / `unsubscribe`.
- **Resilient transport** on the JDK's `java.net.http.HttpClient`: exponential backoff with jitter, `Retry-After` support for HTTP 429, and retries only for idempotent requests on network and 5xx failures.
- **Typed, unchecked errors**: `TesseraApiException`, `TesseraRateLimitException`, `TesseraNetworkException`, `TesseraSerializationException`.
- Runtime dependencies: `jackson-databind` and `jackson-datatype-jsr310` only.

## Install

Maven:

```xml
<dependency>
  <groupId>io.github.a4-stellar</groupId>
  <artifactId>tessera-java-sdk</artifactId>
  <version>0.1.0</version>
</dependency>
```

Gradle (Kotlin DSL):

```kotlin
implementation("io.github.a4-stellar:tessera-java-sdk:0.1.0")
```

## Quick start

```java
import io.tessera.sdk.*;
import io.tessera.sdk.model.*;
import java.net.URI;
import java.time.Duration;

TesseraClient client = TesseraClient.create(TesseraClientConfig.builder()
    .baseUri(URI.create("https://api.tessera.example.com"))
    .timeout(Duration.ofSeconds(15))
    .maxRetries(3)
    .build());

// Blocking
Stats stats = client.getStats();
System.out.println("TVL: $" + stats.tvlUsd());

// Asynchronous
client.listAssetsAsync(new ListAssetsQuery("real_estate", true, null, 20, null))
    .thenCompose(assets -> client.getAssetHoldersAsync(assets.get(0).id(), PageQuery.first(10)))
    .thenAccept(holders -> holders.forEach(h -> System.out.println(h.address() + " " + h.sharePercent() + "%")))
    .join();
```

`TesseraClient` is thread-safe; create one per base URI and share it. Pass your own `HttpClient` through `TesseraClientConfig.builder().httpClient(...)` to control proxies, TLS or executors.

## Kotlin

```kotlin
val client = TesseraClient.create(URI.create("https://api.tessera.example.com"))

val portfolio = client.getHolderPortfolio("GABC…", PortfolioQuery(null, null, 0.05))
println(portfolio.performance().timeWeightedReturnPct())

// With kotlinx-coroutines-jdk8, futures become suspend calls:
val analytics = client.getAssetAnalyticsAsync(1).await()
```

## Streaming events

```java
try (EventSubscription subscription = client.subscribeEvents(List.of("transfer", "dividend"), new TesseraEventListener() {
        @Override public void onEvent(TesseraEvent event) {
            System.out.println(event.ledger() + " " + event.eventType() + " " + event.data());
        }
        @Override public void onError(TesseraException error) {
            log.warn("event stream error", error);
        }
    })) {
    subscription.subscribe("compliance").join();   // add a topic while connected
    subscription.unsubscribe("transfer").join();    // or drop one
    subscription.closeFuture().get(10, TimeUnit.MINUTES);
}
```

The client opens `/v1/ws` (derived from the base URI — `https` becomes `wss` — or set `webSocketUri`), sends one `subscribe:<topic>` frame per topic and decodes each pushed JSON frame into a `TesseraEvent`. A topic matches an event when it appears, case-insensitively, in the event type, contract ID or payload. Fragmented frames are reassembled; a frame that cannot be decoded is reported to `onError` and the stream keeps running.

## Errors

| Exception | When |
| --- | --- |
| `TesseraApiException` | Non-2xx response. `statusCode()`, `errorCode()` (e.g. `not_found`) and `apiMessage()` come from the API's `{ "error", "message" }` body. |
| `TesseraRateLimitException` | HTTP 429 after retries; `retryAfter()` carries the server's hint. |
| `TesseraNetworkException` | DNS, connection, TLS or timeout failure after retries. |
| `TesseraSerializationException` | A body or event frame did not match the expected model. |

Blocking methods throw these directly. Asynchronous methods complete exceptionally with them, wrapped in `CompletionException` when you call `join()`.

Token amounts and cent values (`totalSupply`, `balance`, `valuationCents`, …) are strings to preserve the API's `i128` precision; use `new BigInteger(value)` for arithmetic.

## Build and release

```bash
cd sdk/java
mvn verify                     # compile (-Werror), test, build jar + sources + javadoc
mvn -P release deploy          # sign with GPG and upload to Maven Central (Central Portal)
```

The `release` profile signs every artifact with `maven-gpg-plugin` and publishes through `central-publishing-maven-plugin`. It needs a GPG key and a `central` server entry holding a Central Portal user token in `~/.m2/settings.xml`. Publishing under `io.github.a4-stellar` requires that namespace to be verified for the A4-Stellar GitHub organisation in the Central Portal. `autoPublish` is off, so each upload is reviewed in the portal before release.
