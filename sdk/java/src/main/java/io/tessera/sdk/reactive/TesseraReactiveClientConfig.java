package io.tessera.sdk.reactive;

import io.github.resilience4j.circuitbreaker.CircuitBreaker;
import io.github.resilience4j.circuitbreaker.CircuitBreakerConfig;
import io.github.resilience4j.ratelimiter.RateLimiter;
import io.github.resilience4j.ratelimiter.RateLimiterConfig;
import io.github.resilience4j.retry.Retry;
import io.github.resilience4j.retry.RetryConfig;
import java.net.URI;
import java.time.Duration;
import java.util.Objects;
import org.springframework.web.reactive.function.client.WebClient;

/**
 * Configuration for {@link TesseraReactiveClient}.
 */
public final class TesseraReactiveClientConfig {
    private final URI baseUri;
    private final URI rsocketUri;
    private final int prefetchCount;
    private final Duration timeout;
    private final String authToken;
    private final WebClient webClient;
    private final CircuitBreaker circuitBreaker;
    private final Retry retry;
    private final RateLimiter rateLimiter;

    private TesseraReactiveClientConfig(Builder builder) {
        this.baseUri = Objects.requireNonNull(builder.baseUri, "baseUri must not be null");
        this.rsocketUri = builder.rsocketUri != null
            ? builder.rsocketUri
            : deriveRSocketUri(builder.baseUri);
        this.prefetchCount = builder.prefetchCount > 0 ? builder.prefetchCount : 256;
        this.timeout = builder.timeout != null ? builder.timeout : Duration.ofSeconds(15);
        this.authToken = builder.authToken;
        this.webClient = builder.webClient;

        this.circuitBreaker = builder.circuitBreaker != null
            ? builder.circuitBreaker
            : CircuitBreaker.of("tesseraReactive", CircuitBreakerConfig.custom()
                .failureRateThreshold(50.0f)
                .slidingWindowSize(10)
                .waitDurationInOpenState(Duration.ofSeconds(5))
                .build());

        this.retry = builder.retry != null
            ? builder.retry
            : Retry.of("tesseraReactive", RetryConfig.custom()
                .maxAttempts(3)
                .waitDuration(Duration.ofMillis(500))
                .build());

        this.rateLimiter = builder.rateLimiter != null
            ? builder.rateLimiter
            : RateLimiter.of("tesseraReactive", RateLimiterConfig.custom()
                .limitForPeriod(100)
                .limitRefreshPeriod(Duration.ofSeconds(1))
                .timeoutDuration(Duration.ofSeconds(2))
                .build());
    }

    private static URI deriveRSocketUri(URI base) {
        String scheme = base.getScheme() != null && base.getScheme().equalsIgnoreCase("https") ? "wss" : "ws";
        String host = base.getHost() != null ? base.getHost() : "api";
        int port = base.getPort();
        String portPart = port > 0 ? ":" + port : "";
        String path = base.getPath() != null && !base.getPath().isBlank()
            ? base.getPath().replaceAll("/+$", "")
            : "/v1";
        if (!path.endsWith("/rsocket")) {
            path = path + (path.endsWith("/v1") ? "/rsocket" : "/v1/rsocket");
        }
        return URI.create(scheme + "://" + host + portPart + path);
    }

    public static Builder builder() {
        return new Builder();
    }

    public URI baseUri() { return baseUri; }
    public URI rsocketUri() { return rsocketUri; }
    public int prefetchCount() { return prefetchCount; }
    public Duration timeout() { return timeout; }
    public String authToken() { return authToken; }
    public WebClient webClient() { return webClient; }
    public CircuitBreaker circuitBreaker() { return circuitBreaker; }
    public Retry retry() { return retry; }
    public RateLimiter rateLimiter() { return rateLimiter; }

    public static final class Builder {
        private URI baseUri = URI.create("http://localhost:8080");
        private URI rsocketUri;
        private int prefetchCount = 256;
        private Duration timeout = Duration.ofSeconds(15);
        private String authToken;
        private WebClient webClient;
        private CircuitBreaker circuitBreaker;
        private Retry retry;
        private RateLimiter rateLimiter;

        public Builder baseUri(URI baseUri) {
            this.baseUri = baseUri;
            return this;
        }

        public Builder rsocketUri(URI rsocketUri) {
            this.rsocketUri = rsocketUri;
            return this;
        }

        public Builder prefetchCount(int prefetchCount) {
            this.prefetchCount = prefetchCount;
            return this;
        }

        public Builder timeout(Duration timeout) {
            this.timeout = timeout;
            return this;
        }

        public Builder authToken(String authToken) {
            this.authToken = authToken;
            return this;
        }

        public Builder webClient(WebClient webClient) {
            this.webClient = webClient;
            return this;
        }

        public Builder circuitBreaker(CircuitBreaker circuitBreaker) {
            this.circuitBreaker = circuitBreaker;
            return this;
        }

        public Builder retry(Retry retry) {
            this.retry = retry;
            return this;
        }

        public Builder rateLimiter(RateLimiter rateLimiter) {
            this.rateLimiter = rateLimiter;
            return this;
        }

        public TesseraReactiveClientConfig build() {
            return new TesseraReactiveClientConfig(this);
        }
    }
}
