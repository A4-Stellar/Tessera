package io.tessera.sdk.model;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;
import com.fasterxml.jackson.annotation.JsonProperty;
import java.time.OffsetDateTime;

/**
 * An audit root published to the Stellar ledger.
 */
@JsonIgnoreProperties(ignoreUnknown = true)
public record AnchorRecord(
    @JsonProperty("anchor_id") long anchorId,
    @JsonProperty("root_hash") String rootHash,
    @JsonProperty("start_sequence") long startSequence,
    @JsonProperty("end_sequence") long endSequence,
    @JsonProperty("entry_count") int entryCount,
    @JsonProperty("ledger_sequence") long ledgerSequence,
    @JsonProperty("memo_hash") String memoHash,
    @JsonProperty("contract_storage_key") String contractStorageKey,
    @JsonProperty("tx_hash") String txHash,
    @JsonProperty("published_at") OffsetDateTime publishedAt) {
}
