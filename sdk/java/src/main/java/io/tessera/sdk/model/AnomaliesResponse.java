package io.tessera.sdk.model;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;
import com.fasterxml.jackson.annotation.JsonProperty;
import java.util.List;

/**
 * Active anomaly flags ({@code GET /v1/security/anomalies}).
 */
@JsonIgnoreProperties(ignoreUnknown = true)
public record AnomaliesResponse(
    @JsonProperty("threshold") double threshold,
    @JsonProperty("count") int count,
    @JsonProperty("flags") List<RiskFlag> flags) {

    /** Normalises absent collections to empty, immutable lists. */
    public AnomaliesResponse {
        flags = flags == null ? List.of() : List.copyOf(flags);
    }
}
