package io.tessera.sdk.model;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;
import com.fasterxml.jackson.annotation.JsonProperty;
import java.util.List;

/**
 * Merkle inclusion proof for an audit entry.
 */
@JsonIgnoreProperties(ignoreUnknown = true)
public record MerkleProof(
    @JsonProperty("leaf_index") long leafIndex,
    @JsonProperty("leaf_hash") String leafHash,
    @JsonProperty("steps") List<ProofStep> steps,
    @JsonProperty("root_hash") String rootHash) {

    /** Normalises absent collections to empty, immutable lists. */
    public MerkleProof {
        steps = steps == null ? List.of() : List.copyOf(steps);
    }
}
