package io.tessera.sdk.model;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;
import com.fasterxml.jackson.annotation.JsonProperty;

/**
 * Result of verifying the audit log.
 */
@JsonIgnoreProperties(ignoreUnknown = true)
public record AuditVerificationResult(
    @JsonProperty("is_valid") boolean isValid,
    @JsonProperty("total_entries") int totalEntries,
    @JsonProperty("genesis_hash") String genesisHash,
    @JsonProperty("chain_tip_hash") String chainTipHash,
    @JsonProperty("merkle_root") String merkleRoot,
    @JsonProperty("anchored_root_hash") String anchoredRootHash,
    @JsonProperty("anchor_status") String anchorStatus,
    @JsonProperty("latest_anchor") AnchorRecord latestAnchor,
    @JsonProperty("first_tamper_index") Long firstTamperIndex,
    @JsonProperty("error_detail") String errorDetail) {
}
