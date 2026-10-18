package io.tessera.sdk.model;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;
import com.fasterxml.jackson.annotation.JsonProperty;

/**
 * Platform-wide aggregated metrics ({@code GET /v1/stats}).
 *
 * <p>Token amounts and cent values are strings to keep the API's {@code i128} precision;
 * use {@link java.math.BigInteger} for arithmetic.
 */
@JsonIgnoreProperties(ignoreUnknown = true)
public record Stats(
    @JsonProperty("total_assets") int totalAssets,
    @JsonProperty("active_assets") int activeAssets,
    @JsonProperty("tvl_cents") String tvlCents,
    @JsonProperty("tvl_usd") double tvlUsd,
    @JsonProperty("total_holders") int totalHolders,
    @JsonProperty("total_distributions") int totalDistributions,
    @JsonProperty("last_indexed_ledger") long lastIndexedLedger,
    @JsonProperty("last_updated") String lastUpdated) {
}
