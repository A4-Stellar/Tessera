package io.tessera.sdk.model;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;
import com.fasterxml.jackson.annotation.JsonProperty;

/**
 * Number of compliance records in one jurisdiction.
 */
@JsonIgnoreProperties(ignoreUnknown = true)
public record JurisdictionCount(
    @JsonProperty("jurisdiction") String jurisdiction,
    @JsonProperty("count") int count) {
}
