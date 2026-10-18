package io.tessera.sdk.model;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;
import com.fasterxml.jackson.annotation.JsonProperty;
import java.util.List;

/**
 * A page of audit log entries.
 */
@JsonIgnoreProperties(ignoreUnknown = true)
public record AuditEntriesPage(
    @JsonProperty("entries") List<AuditEntry> entries,
    @JsonProperty("total") int total,
    @JsonProperty("offset") int offset,
    @JsonProperty("limit") int limit) {

    /** Normalises absent collections to empty, immutable lists. */
    public AuditEntriesPage {
        entries = entries == null ? List.of() : List.copyOf(entries);
    }
}
