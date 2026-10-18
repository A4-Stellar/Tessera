package io.tessera.sdk.model;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;
import com.fasterxml.jackson.annotation.JsonProperty;

/**
 * An audit entry with its Merkle inclusion proof.
 */
@JsonIgnoreProperties(ignoreUnknown = true)
public record AuditEntryDetail(
    @JsonProperty("entry") AuditEntry entry,
    @JsonProperty("inclusion_proof") MerkleProof inclusionProof) {
}
