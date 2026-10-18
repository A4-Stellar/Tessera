package io.tessera.sdk;

import java.net.URI;
import java.net.http.HttpClient;
import java.time.Duration;
import java.util.LinkedHashMap;
import java.util.Map;
import java.util.Objects;
import java.util.Optional;

/**
 * Immutable configuration for {@link TesseraClient}. Create one with {@link #builder()}.
 *
 * <pre>{@code
 * TesseraClientConfig config = TesseraClientConfig.builder()
 *     .baseUri(URI.create("https://api.tessera.example.com"))
 *     .timeout(Duration.ofSeconds(15))
 *     .maxRetries(3)
 *     .build();
 * }</pre>
 */
public final class TesseraClientConfig {
    private final URI baseUri;
    private final URI webSocketUri;
    private final Duration timeout;
    private final int maxRetries;
    private final Duration initialBackoff;
    private final Duration maxBackoff;
    private final String authToken;
    private final Map<String, String> headers;
    private final HttpClient httpClient;

    private TesseraClientConfig(Builder builder) {
        this.baseUri = stripTrailingSlash(Objects.requireNonNull(builder.baseUri, "baseUri"));
        String scheme = baseUri.getScheme();
        if (!baseUri.isAbsolute() || !("http".equals(scheme) || "https".equals(scheme)) || baseUri.getHost() == null) {
            throw new IllegalArgumentException("baseUri must be an absolute http(s) URI, got " + builder.baseUri);
        }
        if (builder.maxRetries < 0) {
            throw new IllegalArgumentException("maxRetries cannot be negative");
        }
        if (builder.timeout.isZero() || builder.timeout.isNegative()) {
            throw new IllegalArgumentException("timeout must be positive");
        }
        this.webSocketUri = builder.webSocketUri;
        this.timeout = builder.timeout;
        this.maxRetries = builder.maxRetries;
        this.initialBackoff = builder.initialBackoff;
        this.maxBackoff = builder.maxBackoff;
        this.authToken = builder.authToken;
        this.headers = Map.copyOf(builder.headers);
        this.httpClient = builder.httpClient;
    }

    /** Returns a builder with production defaults ({@code http://localhost:8080}, 30s timeout, 3 retries). */
    public static Builder builder() {
        return new Builder();
    }

    /** Base URI of the REST API, without a trailing slash. */
    public URI baseUri() {
        return baseUri;
    }

    /**
     * WebSocket endpoint for the event stream. Defaults to {@code /v1/ws} on the base URI's host,
     * with {@code https} mapped to {@code wss}.
     */
    public URI webSocketUri() {
        if (webSocketUri != null) {
            return webSocketUri;
        }
        String scheme = "https".equals(baseUri.getScheme()) ? "wss" : "ws";
        String port = baseUri.getPort() == -1 ? "" : ":" + baseUri.getPort();
        return URI.create(scheme + "://" + baseUri.getHost() + port + "/v1/ws");
    }

    /** Per-request timeout. */
    public Duration timeout() {
        return timeout;
    }

    /** Maximum retries for idempotent requests and rate-limited responses. */
    public int maxRetries() {
        return maxRetries;
    }

    /** First retry delay; doubled on each attempt up to {@link #maxBackoff()}. */
    public Duration initialBackoff() {
        return initialBackoff;
    }

    /** Upper bound for the retry delay. */
    public Duration maxBackoff() {
        return maxBackoff;
    }

    /** Bearer token sent on REST and WebSocket requests, if configured. */
    public Optional<String> authToken() {
        return Optional.ofNullable(authToken);
    }

    /** Extra headers added to every request. */
    public Map<String, String> headers() {
        return headers;
    }

    /** Caller-supplied {@link HttpClient}, if any. */
    Optional<HttpClient> httpClient() {
        return Optional.ofNullable(httpClient);
    }

    private static URI stripTrailingSlash(URI uri) {
        String text = uri.toString();
        return text.endsWith("/") ? URI.create(text.substring(0, text.length() - 1)) : uri;
    }

    /** Mutable builder for {@link TesseraClientConfig}. */
    public static final class Builder {
        private URI baseUri = URI.create("http://localhost:8080");
        private URI webSocketUri;
        private Duration timeout = Duration.ofSeconds(30);
        private int maxRetries = 3;
        private Duration initialBackoff = Duration.ofMillis(200);
        private Duration maxBackoff = Duration.ofSeconds(3);
        private String authToken;
        private final Map<String, String> headers = new LinkedHashMap<>();
        private HttpClient httpClient;

        private Builder() {
        }

        /** Base URI of the Tessera REST API. */
        public Builder baseUri(URI baseUri) {
            this.baseUri = Objects.requireNonNull(baseUri, "baseUri");
            return this;
        }

        /** Overrides the WebSocket endpoint derived from the base URI. */
        public Builder webSocketUri(URI webSocketUri) {
            this.webSocketUri = webSocketUri;
            return this;
        }

        /** Per-request timeout (default 30 seconds). */
        public Builder timeout(Duration timeout) {
            this.timeout = Objects.requireNonNull(timeout, "timeout");
            return this;
        }

        /** Maximum retries (default 3). */
        public Builder maxRetries(int maxRetries) {
            this.maxRetries = maxRetries;
            return this;
        }

        /** Exponential backoff bounds (default 200ms, capped at 3s). */
        public Builder backoff(Duration initial, Duration max) {
            this.initialBackoff = Objects.requireNonNull(initial, "initial");
            this.maxBackoff = Objects.requireNonNull(max, "max");
            return this;
        }

        /** Bearer token for protected routes such as {@code /metrics}. */
        public Builder authToken(String authToken) {
            this.authToken = authToken;
            return this;
        }

        /** Adds a header to every outbound request. */
        public Builder header(String name, String value) {
            this.headers.put(Objects.requireNonNull(name, "name"), Objects.requireNonNull(value, "value"));
            return this;
        }

        /**
         * Uses a caller-managed {@link HttpClient} (custom proxy, SSL context or executor).
         * Its own connect timeout applies; {@link #timeout(Duration)} still bounds each request.
         */
        public Builder httpClient(HttpClient httpClient) {
            this.httpClient = httpClient;
            return this;
        }

        /** Validates and builds the configuration. */
        public TesseraClientConfig build() {
            return new TesseraClientConfig(this);
        }
    }
}
