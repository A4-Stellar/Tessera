package io.tessera.sdk.model;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;
import com.fasterxml.jackson.annotation.JsonProperty;
import java.util.List;

/**
 * Diagnostic events captured for an asset.
 */
@JsonIgnoreProperties(ignoreUnknown = true)
public record AssetEvents(
    @JsonProperty("asset_id") long assetId,
    @JsonProperty("diagnostics") List<DiagnosticEvent> diagnostics) {

    /** Normalises absent collections to empty, immutable lists. */
    public AssetEvents {
        diagnostics = diagnostics == null ? List.of() : List.copyOf(diagnostics);
    }
}
