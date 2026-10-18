package io.tessera.sdk.model;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;
import com.fasterxml.jackson.annotation.JsonProperty;
import java.util.List;

/**
 * Portfolio analytics for an investor address: TWR, NPV, IRR and a time series.
 */
@JsonIgnoreProperties(ignoreUnknown = true)
public record HolderPortfolio(
    @JsonProperty("address") String address,
    @JsonProperty("summary") PortfolioSummary summary,
    @JsonProperty("holdings") List<PortfolioHolding> holdings,
    @JsonProperty("dividends") List<PortfolioDividendPayout> dividends,
    @JsonProperty("performance") PortfolioPerformance performance,
    @JsonProperty("time_series") List<PortfolioTimeSeriesPoint> timeSeries) {

    /** Normalises absent collections to empty, immutable lists. */
    public HolderPortfolio {
        holdings = holdings == null ? List.of() : List.copyOf(holdings);
        dividends = dividends == null ? List.of() : List.copyOf(dividends);
        timeSeries = timeSeries == null ? List.of() : List.copyOf(timeSeries);
    }
}
