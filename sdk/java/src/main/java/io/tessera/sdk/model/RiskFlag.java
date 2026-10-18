package io.tessera.sdk.model;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;
import com.fasterxml.jackson.annotation.JsonProperty;

/**
 * One account flagged by the anomaly detector.
 */
@JsonIgnoreProperties(ignoreUnknown = true)
public record RiskFlag(
    @JsonProperty("account") String account,
    @JsonProperty("metric") String metric,
    @JsonProperty("peak_z_score") double peakZScore,
    @JsonProperty("latest_z_score") double latestZScore,
    @JsonProperty("observed") double observed,
    @JsonProperty("baseline_mean") double baselineMean,
    @JsonProperty("baseline_std") double baselineStd,
    @JsonProperty("first_seen") long firstSeen,
    @JsonProperty("last_seen") long lastSeen,
    @JsonProperty("trigger_count") long triggerCount) {
}
