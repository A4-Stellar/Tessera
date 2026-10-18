package io.tessera.sdk.model;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;
import com.fasterxml.jackson.annotation.JsonProperty;

/**
 * A holder of an asset; list responses are sorted by balance descending.
 *
 * <p>Token amounts and cent values are strings to keep the API's {@code i128} precision;
 * use {@link java.math.BigInteger} for arithmetic.
 */
@JsonIgnoreProperties(ignoreUnknown = true)
public record Holder(
    @JsonProperty("address") String address,
    @JsonProperty("balance") String balance,
    @JsonProperty("share_percent") double sharePercent) {
}
