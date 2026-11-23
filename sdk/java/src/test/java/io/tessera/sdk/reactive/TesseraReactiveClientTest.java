package io.tessera.sdk.reactive;

import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.databind.node.JsonNodeFactory;
import io.github.resilience4j.circuitbreaker.CircuitBreaker;
import io.github.resilience4j.circuitbreaker.CircuitBreakerConfig;
import io.github.resilience4j.ratelimiter.RateLimiter;
import io.github.resilience4j.ratelimiter.RateLimiterConfig;
import io.github.resilience4j.retry.Retry;
import io.github.resilience4j.retry.RetryConfig;
import io.rsocket.Payload;
import io.rsocket.RSocket;
import io.rsocket.util.DefaultPayload;
import io.tessera.sdk.model.Asset;
import io.tessera.sdk.model.Holder;
import io.tessera.sdk.model.TesseraEvent;
import java.net.URI;
import java.time.Duration;
import java.util.List;
import java.util.concurrent.atomic.AtomicInteger;
import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.api.DisplayName;
import org.junit.jupiter.api.Test;
import org.springframework.web.reactive.function.client.WebClient;
import reactor.core.publisher.Flux;
import reactor.core.publisher.Mono;
import reactor.test.StepVerifier;

import static org.junit.jupiter.api.Assertions.*;

public class TesseraReactiveClientTest {
    private ObjectMapper objectMapper;
    private Asset sampleAsset;
    private List<Holder> sampleHolders;
    private List<TesseraEvent> sampleEvents;

    @BeforeEach
    void setUp() {
        objectMapper = new ObjectMapper();
        sampleAsset = new Asset(
            1L,
            "CBMCWLSQSWUTLUJFCNBHNBSXMUM3XU7NAQ5TSNERW4HA4ZZBYHLG4ECZ",
            "GAIQGTOBTTLLDJ4SWGGESM7UWJ2DI4K3ZNHUSHPDKJL2IE5FKY3BSRAA",
            "Manhattan Loft",
            "MLOFT",
            "real_estate",
            "A tokenized loft in Manhattan",
            "500000000",
            5000000.0,
            2,
            "1000000",
            2,
            true,
            false,
            "CBUERYDM7DXTZLLKDBRJKUBPFJ7M4OSUN4T7XKUARU345RLXNAIQD2IU",
            3502885L,
            3502890L,
            null
        );

        sampleHolders = List.of(
            new Holder("GBBD...TESSERA", "900000", 90.0),
            new Holder("GCXK...INVESTOR", "100000", 10.0)
        );

        sampleEvents = List.of(
            new TesseraEvent(1L, "CBMC...ECZ", "transfer", 3502891L, "2026-11-21T18:00:00Z", JsonNodeFactory.instance.objectNode().put("amount", 25000)),
            new TesseraEvent(2L, "CBMC...ECZ", "transfer", 3502892L, "2026-11-21T18:01:00Z", JsonNodeFactory.instance.objectNode().put("amount", 50000))
        );
    }

    @Test
    @DisplayName("StepVerifier verifies Mono<Asset> emission and completion")
    void testGetAssetMono() {
        TesseraReactiveClient client = new TesseraReactiveClient(
            TesseraReactiveClientConfig.builder()
                .baseUri(URI.create("http://localhost:8080"))
                .prefetchCount(128)
                .build()
        ) {
            @Override
            public Mono<Asset> getAsset(long assetId, String fields) {
                return decorate(Mono.just(sampleAsset));
            }
        };

        StepVerifier.create(client.getAsset(1L))
            .expectNextMatches(asset -> asset.id() == 1L && "MLOFT".equals(asset.symbol()) && "Manhattan Loft".equals(asset.name()))
            .verifyComplete();
    }

    @Test
    @DisplayName("StepVerifier verifies Flux<Holder> stream with backpressure")
    void testGetAssetHoldersFlux() {
        TesseraReactiveClient client = new TesseraReactiveClient(
            TesseraReactiveClientConfig.builder()
                .baseUri(URI.create("http://localhost:8080"))
                .prefetchCount(64)
                .build()
        ) {
            @Override
            public Flux<Holder> getAssetHolders(long assetId) {
                return decorate(Flux.fromIterable(sampleHolders));
            }
        };

        StepVerifier.create(client.getAssetHolders(1L))
            .expectNextMatches(h -> "GBBD...TESSERA".equals(h.address()) && h.sharePercent() == 90.0)
            .expectNextMatches(h -> "GCXK...INVESTOR".equals(h.address()) && h.sharePercent() == 10.0)
            .verifyComplete();
    }

    @Test
    @DisplayName("StepVerifier verifies RSocket requestStream Flux<TesseraEvent>")
    void testStreamEventsFlux() {
        TesseraReactiveClient client = new TesseraReactiveClient(
            TesseraReactiveClientConfig.builder()
                .baseUri(URI.create("http://localhost:8080"))
                .rsocketUri(URI.create("ws://localhost:8080/v1/rsocket"))
                .prefetchCount(256)
                .build()
        ) {
            @Override
            public Flux<TesseraEvent> streamEvents(List<String> topics) {
                return decorate(Flux.fromIterable(sampleEvents));
            }
        };

        StepVerifier.create(client.streamEvents("transfer"))
            .expectNextMatches(e -> e.id() == 1L && "transfer".equals(e.eventType()))
            .expectNextMatches(e -> e.id() == 2L && e.ledger() == 3502892L)
            .verifyComplete();
    }

    @Test
    @DisplayName("StepVerifier verifies Backpressure.BUFFER strategy with prefetch count")
    void testBackpressureBufferStrategy() {
        int prefetch = 10;
        TesseraReactiveClient client = new TesseraReactiveClient(
            TesseraReactiveClientConfig.builder()
                .baseUri(URI.create("http://localhost:8080"))
                .prefetchCount(prefetch)
                .build()
        );

        // Generate 50 items and verify backpressure buffering
        Flux<Integer> buffered = client.decorate(Flux.range(1, 50));

        StepVerifier.create(buffered, 5) // Request only 5 items initially
            .expectNext(1, 2, 3, 4, 5)
            .thenRequest(5) // Request next 5 items
            .expectNext(6, 7, 8, 9, 10)
            .thenCancel()
            .verify();
    }

    @Test
    @DisplayName("Resilience4j CircuitBreaker, Retry, and RateLimiter decorate Mono")
    void testResilience4jDecorators() {
        AtomicInteger attempts = new AtomicInteger(0);

        CircuitBreaker cb = CircuitBreaker.of("testCB", CircuitBreakerConfig.ofDefaults());
        Retry retry = Retry.of("testRetry", RetryConfig.custom().maxAttempts(3).waitDuration(Duration.ofMillis(10)).build());
        RateLimiter rl = RateLimiter.of("testRL", RateLimiterConfig.ofDefaults());

        TesseraReactiveClient client = new TesseraReactiveClient(
            TesseraReactiveClientConfig.builder()
                .baseUri(URI.create("http://localhost:8080"))
                .circuitBreaker(cb)
                .retry(retry)
                .rateLimiter(rl)
                .build()
        ) {
            @Override
            public Mono<Asset> getAsset(long assetId, String fields) {
                return decorate(Mono.defer(() -> {
                    if (attempts.incrementAndGet() < 3) {
                        return Mono.error(new RuntimeException("Transient failure " + attempts.get()));
                    }
                    return Mono.just(sampleAsset);
                }));
            }
        };

        // Retry should recover after 2 failures and succeed on attempt 3
        StepVerifier.create(client.getAsset(1L))
            .expectNextMatches(a -> a.id() == 1L)
            .verifyComplete();

        assertEquals(3, attempts.get());
        assertEquals(0, cb.getMetrics().getNumberOfFailedCalls());
    }
}
