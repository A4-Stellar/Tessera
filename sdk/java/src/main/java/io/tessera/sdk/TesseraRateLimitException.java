package io.tessera.sdk;

import java.time.Duration;
import java.util.Optional;

/** The API responded with HTTP 429 and retries were exhausted. */
public final class TesseraRateLimitException extends TesseraApiException {
    private static final long serialVersionUID = 1L;

    private final transient Duration retryAfter;

    /** Creates a rate-limit exception. */
    public TesseraRateLimitException(Duration retryAfter, String apiMessage) {
        super(429, "rate_limited", apiMessage);
        this.retryAfter = retryAfter;
    }

    /** Delay advertised by the {@code Retry-After} header, when present. */
    public Optional<Duration> retryAfter() {
        return Optional.ofNullable(retryAfter);
    }
}
