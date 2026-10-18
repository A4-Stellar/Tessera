package io.tessera.sdk.model;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;
import com.fasterxml.jackson.annotation.JsonProperty;

/**
 * One step of a Merkle inclusion proof.
 */
@JsonIgnoreProperties(ignoreUnknown = true)
public record ProofStep(
    @JsonProperty("sibling_hash") String siblingHash,
    @JsonProperty("is_sibling_left") boolean isSiblingLeft) {
}
