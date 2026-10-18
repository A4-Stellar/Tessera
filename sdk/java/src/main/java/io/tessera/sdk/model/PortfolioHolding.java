package io.tessera.sdk.model;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;
import com.fasterxml.jackson.annotation.JsonProperty;

/**
 * One asset position in a portfolio.
 *
 * <p>Token amounts and cent values are strings to keep the API's {@code i128} precision;
 * use {@link java.math.BigInteger} for arithmetic.
 */
@JsonIgnoreProperties(ignoreUnknown = true)
public record PortfolioHolding(
    @JsonProperty("asset_id") long assetId,
    @JsonProperty("asset_name") String assetName,
    @JsonProperty("symbol") String symbol,
    @JsonProperty("asset_type") String assetType,
    @JsonProperty("balance") String balance,
    @JsonProperty("decimals") int decimals,
    @JsonProperty("share_percent") double sharePercent,
    @JsonProperty("unit_price_usd") double unitPriceUsd,
    @JsonProperty("market_value_usd") double marketValueUsd,
    @JsonProperty("dividends_received_usd") double dividendsReceivedUsd) {
}
