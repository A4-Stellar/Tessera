package io.tessera.sdk.model;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;
import com.fasterxml.jackson.annotation.JsonProperty;

/**
 * Outcome of {@code POST /v1/admin/dlq/retry}.
 */
@JsonIgnoreProperties(ignoreUnknown = true)
public record RetryReport(
    @JsonProperty("retried") int retried,
    @JsonProperty("resolved") int resolved,
    @JsonProperty("still_quarantined") int stillQuarantined) {
}
