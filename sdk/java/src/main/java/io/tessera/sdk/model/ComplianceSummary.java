package io.tessera.sdk.model;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;
import com.fasterxml.jackson.annotation.JsonProperty;
import java.util.List;

/**
 * Aggregate allowlist summary for an asset.
 */
@JsonIgnoreProperties(ignoreUnknown = true)
public record ComplianceSummary(
    @JsonProperty("total_records") int totalRecords,
    @JsonProperty("approved") int approved,
    @JsonProperty("suspended") int suspended,
    @JsonProperty("rejected") int rejected,
    @JsonProperty("pending") int pending,
    @JsonProperty("with_expiry") int withExpiry,
    @JsonProperty("jurisdictions") List<JurisdictionCount> jurisdictions) {

    /** Normalises absent collections to empty, immutable lists. */
    public ComplianceSummary {
        jurisdictions = jurisdictions == null ? List.of() : List.copyOf(jurisdictions);
    }
}
