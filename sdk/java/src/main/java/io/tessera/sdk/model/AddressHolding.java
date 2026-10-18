package io.tessera.sdk.model;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;
import com.fasterxml.jackson.annotation.JsonProperty;

/**
 * One asset held by an investor address.
 *
 * <p>Token amounts and cent values are strings to keep the API's {@code i128} precision;
 * use {@link java.math.BigInteger} for arithmetic.
 */
@JsonIgnoreProperties(ignoreUnknown = true)
public record AddressHolding(
    @JsonProperty("address") String address,
    @JsonProperty("asset_id") long assetId,
    @JsonProperty("asset_name") String assetName,
    @JsonProperty("symbol") String symbol,
    @JsonProperty("balance") String balance,
    @JsonProperty("share_percent") double sharePercent) {
}
