package io.tessera.sdk.reactive;

import com.fasterxml.jackson.core.JsonProcessingException;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.databind.json.JsonMapper;
import com.fasterxml.jackson.datatype.jsr310.JavaTimeModule;
import io.github.resilience4j.circuitbreaker.CircuitBreaker;
import io.github.resilience4j.ratelimiter.RateLimiter;
import io.github.resilience4j.reactor.circuitbreaker.operator.CircuitBreakerOperator;
import io.github.resilience4j.reactor.ratelimiter.operator.RateLimiterOperator;
import io.github.resilience4j.reactor.retry.operator.RetryOperator;
import io.github.resilience4j.retry.Retry;
import io.rsocket.Payload;
import io.rsocket.RSocket;
import io.rsocket.core.RSocketConnector;
import io.rsocket.transport.netty.client.WebsocketClientTransport;
import io.rsocket.util.DefaultPayload;
import io.tessera.sdk.PageQuery;
import io.tessera.sdk.TesseraApiException;
import io.tessera.sdk.TesseraSerializationException;
import io.tessera.sdk.model.Asset;
import io.tessera.sdk.model.Holder;
import io.tessera.sdk.model.TesseraEvent;
import java.net.URI;
import java.util.Arrays;
import java.util.List;
import java.util.Objects;
import java.util.function.Function;
import org.springframework.http.HttpHeaders;
import org.springframework.http.HttpStatusCode;
import org.springframework.http.MediaType;
import org.springframework.web.reactive.function.client.ClientResponse;
import org.springframework.web.reactive.function.client.WebClient;
import reactor.core.publisher.BufferOverflowStrategy;
import reactor.core.publisher.Flux;
import reactor.core.publisher.Mono;

/**
 * Non-blocking, reactive client for the Tessera API and event stream.
 *
 * <p>Backed by Spring WebFlux {@link WebClient} and RSocket {@code requestStream}
 * for streaming events, with configurable {@code Backpressure.BUFFER} strategy
 * and decorated with Resilience4j {@link CircuitBreaker}, {@link Retry}, and {@link RateLimiter}.
 */
public class TesseraReactiveClient implements AutoCloseable {
    private final WebClient webClient;
    private final TesseraReactiveClientConfig config;
    private final ObjectMapper objectMapper;
    private final CircuitBreaker circuitBreaker;
    private final Retry retry;
    private final RateLimiter rateLimiter;
    private volatile Mono<RSocket> rsocketMono;

    public TesseraReactiveClient(TesseraReactiveClientConfig config) {
        this.config = Objects.requireNonNull(config, "config must not be null");
        this.objectMapper = JsonMapper.builder()
            .addModule(new JavaTimeModule())
            .build();

        this.circuitBreaker = config.circuitBreaker();
        this.retry = config.retry();
        this.rateLimiter = config.rateLimiter();

        if (config.webClient() != null) {
            this.webClient = config.webClient();
        } else {
            WebClient.Builder builder = WebClient.builder()
                .baseUrl(config.baseUri().toString().replaceAll("/+$", "") + "/v1")
                .defaultHeader(HttpHeaders.ACCEPT, MediaType.APPLICATION_JSON_VALUE);

            if (config.authToken() != null && !config.authToken().isBlank()) {
                builder.defaultHeader(HttpHeaders.AUTHORIZATION, "Bearer " + config.authToken());
            }

            this.webClient = builder.build();
        }
    }

    public static TesseraReactiveClient create(URI baseUri) {
        return new TesseraReactiveClient(TesseraReactiveClientConfig.builder()
            .baseUri(baseUri)
            .build());
    }

    public static TesseraReactiveClient create(TesseraReactiveClientConfig config) {
        return new TesseraReactiveClient(config);
    }

    /**
     * Decorates a {@link Mono} with Resilience4j CircuitBreaker, RateLimiter, and Retry.
     */
    protected <T> Mono<T> decorate(Mono<T> source) {
        return source
            .transformDeferred(CircuitBreakerOperator.of(circuitBreaker))
            .transformDeferred(RateLimiterOperator.of(rateLimiter))
            .transformDeferred(RetryOperator.of(retry));
    }

    /**
     * Decorates a {@link Flux} with Resilience4j CircuitBreaker, RateLimiter, Retry,
     * and applies the {@code Backpressure.BUFFER} strategy with the configured prefetch count.
     */
    protected <T> Flux<T> decorate(Flux<T> source) {
        return source
            .onBackpressureBuffer(
                config.prefetchCount(),
                BufferOverflowStrategy.DROP_OLDEST
            )
            .transformDeferred(CircuitBreakerOperator.of(circuitBreaker))
            .transformDeferred(RateLimiterOperator.of(rateLimiter))
            .transformDeferred(RetryOperator.of(retry));
    }

    /**
     * {@code GET /v1/assets/{id}} — retrieves detailed asset record by id.
     */
    public Mono<Asset> getAsset(long assetId) {
        return getAsset(assetId, null);
    }

    /**
     * {@code GET /v1/assets/{id}?fields=...} — retrieves detailed asset with optional sparse fieldset.
     */
    public Mono<Asset> getAsset(long assetId, String fields) {
        return decorate(
            webClient.get()
                .uri(uriBuilder -> {
                    uriBuilder.path("/assets/{id}");
                    if (fields != null && !fields.isBlank()) {
                        uriBuilder.queryParam("fields", fields);
                    }
                    return uriBuilder.build(assetId);
                })
                .retrieve()
                .onStatus(HttpStatusCode::isError, this::handleErrorResponse)
                .bodyToMono(Asset.class)
                .timeout(config.timeout())
        );
    }

    /**
     * {@code GET /v1/assets/{id}/holders} — streams asset holders ordered by balance descending.
     */
    public Flux<Holder> getAssetHolders(long assetId) {
        return getAssetHolders(assetId, null);
    }

    /**
     * {@code GET /v1/assets/{id}/holders} — streams asset holders with pagination.
     */
    public Flux<Holder> getAssetHolders(long assetId, PageQuery page) {
        return decorate(
            webClient.get()
                .uri(uriBuilder -> {
                    uriBuilder.path("/assets/{id}/holders");
                    if (page != null) {
                        if (page.limit() != null) uriBuilder.queryParam("limit", page.limit());
                        if (page.offset() != null) uriBuilder.queryParam("offset", page.offset());
                        if (page.cursor() != null) uriBuilder.queryParam("cursor", page.cursor());
                    }
                    return uriBuilder.build(assetId);
                })
                .retrieve()
                .onStatus(HttpStatusCode::isError, this::handleErrorResponse)
                .bodyToFlux(Holder.class)
                .timeout(config.timeout())
        );
    }

    /**
     * Connects to the RSocket endpoint ({@code ws://api/v1/rsocket}) and uses the
     * {@code requestStream} interaction model to stream live contract events.
     *
     * <p>Applies {@code Backpressure.BUFFER} strategy with configurable prefetch count
     * and resilience4j decorators.
     */
    public Flux<TesseraEvent> streamEvents(String... topics) {
        return streamEvents(Arrays.asList(topics));
    }

    /**
     * Connects to the RSocket endpoint and streams events matching the requested topics.
     */
    public Flux<TesseraEvent> streamEvents(List<String> topics) {
        String topicPayload = topics == null || topics.isEmpty()
            ? "all"
            : String.join(",", topics);

        Flux<TesseraEvent> eventStream = getOrCreateRSocket()
            .flatMapMany(rsocket -> rsocket.requestStream(DefaultPayload.create(topicPayload)))
            .map(this::decodePayload);

        return decorate(eventStream);
    }

    private synchronized Mono<RSocket> getOrCreateRSocket() {
        if (rsocketMono == null) {
            URI uri = config.rsocketUri();
            WebsocketClientTransport transport = WebsocketClientTransport.create(uri);
            rsocketMono = RSocketConnector.create()
                .connect(transport)
                .cache();
        }
        return rsocketMono;
    }

    private TesseraEvent decodePayload(Payload payload) {
        try {
            String json = payload.getDataUtf8();
            return objectMapper.readValue(json, TesseraEvent.class);
        } catch (JsonProcessingException e) {
            throw new TesseraSerializationException("Failed to deserialize event from RSocket payload: " + e.getMessage(), e);
        } finally {
            payload.release();
        }
    }

    private Mono<Throwable> handleErrorResponse(ClientResponse response) {
        return response.bodyToMono(String.class)
            .defaultIfEmpty("")
            .flatMap(body -> Mono.error(new TesseraApiException(
                response.statusCode().value(),
                "API request failed with status " + response.statusCode().value(),
                body
            )));
    }

    public TesseraReactiveClientConfig config() {
        return config;
    }

    public CircuitBreaker circuitBreaker() {
        return circuitBreaker;
    }

    public Retry retry() {
        return retry;
    }

    public RateLimiter rateLimiter() {
        return rateLimiter;
    }

    @Override
    public void close() {
        if (rsocketMono != null) {
            rsocketMono.subscribe(
                RSocket::dispose,
                err -> {}
            );
        }
    }
}
