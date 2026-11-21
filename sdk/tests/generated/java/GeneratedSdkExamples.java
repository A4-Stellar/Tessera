package io.tessera.sdk.generated;

import io.tessera.sdk.PageQuery;
import io.tessera.sdk.model.Asset;
import io.tessera.sdk.model.Holder;
import io.tessera.sdk.model.TesseraEvent;
import io.tessera.sdk.reactive.TesseraReactiveClient;
import io.tessera.sdk.reactive.TesseraReactiveClientConfig;
import java.net.URI;
import java.time.Duration;
import reactor.core.publisher.Flux;
import reactor.core.publisher.Mono;

/**
 * Example demonstrating TesseraReactiveClient usage:
 * Non-blocking Spring WebFlux WebClient, RSocket requestStream event streaming,
 * Backpressure.BUFFER strategy with configurable prefetch, and Resilience4j decorators.
 */
public class GeneratedSdkExamples {

    public static void main(String[] args) {
        String apiBase = System.getenv("NEXT_PUBLIC_API_BASE_URL");
        if (apiBase == null || apiBase.isBlank()) {
            apiBase = "http://localhost:8080";
        }

        // 1. Configure the reactive client
        TesseraReactiveClientConfig config = TesseraReactiveClientConfig.builder()
            .baseUri(URI.create(apiBase))
            .rsocketUri(URI.create(apiBase.replace(/^http/, "ws") + "/v1/rsocket"))
            .prefetchCount(256)
            .timeout(Duration.ofSeconds(15))
            .build();

        try (TesseraReactiveClient client = TesseraReactiveClient.create(config)) {
            // 2. Non-blocking Mono<Asset> query
            Mono<Asset> assetMono = client.getAsset(1L);
            assetMono.subscribe(
                asset -> System.out.println("Asset retrieved: " + asset.name() + " (" + asset.symbol() + ")"),
                error -> System.err.println("Failed to fetch asset: " + error.getMessage())
            );

            // 3. Non-blocking Flux<Holder> stream with Backpressure.BUFFER
            Flux<Holder> holdersFlux = client.getAssetHolders(1L, PageQuery.first(10));
            holdersFlux.subscribe(
                holder -> System.out.println("Holder: " + holder.address() + " - " + holder.sharePercent() + "%"),
                error -> System.err.println("Holders stream error: " + error.getMessage())
            );

            // 4. Real-time RSocket requestStream Flux<TesseraEvent>
            Flux<TesseraEvent> eventStream = client.streamEvents("transfer", "dividend");
            eventStream
                .take(Duration.ofSeconds(10))
                .subscribe(
                    event -> System.out.println("Event: " + event.eventType() + " on ledger " + event.ledger()),
                    error -> System.err.println("RSocket event stream error: " + error.getMessage())
                );
        }
    }
}
