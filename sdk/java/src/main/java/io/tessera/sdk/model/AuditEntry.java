package io.tessera.sdk.model;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;
import com.fasterxml.jackson.annotation.JsonProperty;
import com.fasterxml.jackson.databind.JsonNode;
import java.time.OffsetDateTime;

/**
 * An append-only audit log entry.
 */
@JsonIgnoreProperties(ignoreUnknown = true)
public record AuditEntry(
    @JsonProperty("sequence") long sequence,
    @JsonProperty("timestamp") OffsetDateTime timestamp,
    @JsonProperty("action") JsonNode action,
    @JsonProperty("actor") String actor,
    @JsonProperty("target") String target,
    @JsonProperty("payload") JsonNode payload,
    @JsonProperty("prev_hash") String prevHash,
    @JsonProperty("entry_hash") String entryHash) {
}
