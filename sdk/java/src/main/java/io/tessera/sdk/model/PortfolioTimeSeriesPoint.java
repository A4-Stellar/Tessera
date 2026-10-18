package io.tessera.sdk.model;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;
import com.fasterxml.jackson.annotation.JsonProperty;

/**
 * A chronological portfolio point for charting.
 */
@JsonIgnoreProperties(ignoreUnknown = true)
public record PortfolioTimeSeriesPoint(
    @JsonProperty("timestamp") String timestamp,
    @JsonProperty("ledger") long ledger,
    @JsonProperty("portfolio_value_usd") double portfolioValueUsd,
    @JsonProperty("cash_flow_usd") double cashFlowUsd,
    @JsonProperty("cumulative_invested_usd") double cumulativeInvestedUsd,
    @JsonProperty("cumulative_dividends_usd") double cumulativeDividendsUsd,
    @JsonProperty("period_return") double periodReturn,
    @JsonProperty("cumulative_twr_pct") double cumulativeTwrPct) {
}
