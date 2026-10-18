package io.tessera.sdk;

/** Cap-table export formats of {@code GET /v1/assets/{id}/export}. */
public enum ExportFormat {
    /** Comma-separated values (default). */
    CSV("csv"),
    /** Apache Parquet. */
    PARQUET("parquet");

    private final String wireName;

    ExportFormat(String wireName) {
        this.wireName = wireName;
    }

    String wireName() {
        return wireName;
    }
}
