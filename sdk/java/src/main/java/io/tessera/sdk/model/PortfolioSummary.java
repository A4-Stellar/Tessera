package io.tessera.sdk.model;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;
import com.fasterxml.jackson.annotation.JsonProperty;

/**
 * Aggregate valuation and cash-flow metrics of a portfolio.
 */
@JsonIgnoreProperties(ignoreUnknown = true)
public record PortfolioSummary(
    @JsonProperty("total_valuation_usd") double totalValuationUsd,
    @JsonProperty("total_assets_held") int totalAssetsHeld,
    @JsonProperty("total_cash_inflows_usd") double totalCashInflowsUsd,
    @JsonProperty("total_cash_outflows_usd") double totalCashOutflowsUsd,
    @JsonProperty("net_invested_capital_usd") double netInvestedCapitalUsd,
    @JsonProperty("total_dividends_usd") double totalDividendsUsd,
    @JsonProperty("total_unrealized_gain_usd") double totalUnrealizedGainUsd) {
}
