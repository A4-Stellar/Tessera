package io.tessera.sdk.model;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;
import com.fasterxml.jackson.annotation.JsonProperty;

/**
 * Holder concentration (Gini, HHI) and 24h transaction velocity for an asset.
 *
 * <p>Token amounts and cent values are strings to keep the API's {@code i128} precision;
 * use {@link java.math.BigInteger} for arithmetic.
 */
@JsonIgnoreProperties(ignoreUnknown = true)
public record AssetAnalytics(
    @JsonProperty("asset_id") long assetId,
    @JsonProperty("holder_count") int holderCount,
    @JsonProperty("gini_coefficient") Double giniCoefficient,
    @JsonProperty("herfindahl_hirschman_index") Double herfindahlHirschmanIndex,
    @JsonProperty("volume_24h") String volume24h,
    @JsonProperty("total_supply") String totalSupply,
    @JsonProperty("velocity_24h") Double velocity24h,
    @JsonProperty("window_start") String windowStart,
    @JsonProperty("window_end") String windowEnd) {
}
