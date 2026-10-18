package io.tessera.sdk.model;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;
import com.fasterxml.jackson.annotation.JsonProperty;

/**
 * Rate-of-return metrics of a portfolio.
 */
@JsonIgnoreProperties(ignoreUnknown = true)
public record PortfolioPerformance(
    @JsonProperty("time_weighted_return") double timeWeightedReturn,
    @JsonProperty("time_weighted_return_pct") double timeWeightedReturnPct,
    @JsonProperty("net_present_value") double netPresentValue,
    @JsonProperty("internal_rate_of_return") Double internalRateOfReturn,
    @JsonProperty("internal_rate_of_return_pct") Double internalRateOfReturnPct,
    @JsonProperty("discount_rate") double discountRate) {
}
