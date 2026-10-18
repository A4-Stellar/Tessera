package io.tessera.sdk.model;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;
import com.fasterxml.jackson.annotation.JsonProperty;

/**
 * Compliance status of an address within one asset.
 *
 * <p>Token amounts and cent values are strings to keep the API's {@code i128} precision;
 * use {@link java.math.BigInteger} for arithmetic.
 */
@JsonIgnoreProperties(ignoreUnknown = true)
public record AddressCompliance(
    @JsonProperty("address") String address,
    @JsonProperty("asset_id") long assetId,
    @JsonProperty("asset_name") String assetName,
    @JsonProperty("symbol") String symbol,
    @JsonProperty("balance") String balance,
    @JsonProperty("status") String status,
    @JsonProperty("allowed") boolean allowed) {
}
