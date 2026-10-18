package io.tessera.sdk;

/**
 * Filters for {@code GET /v1/assets}. All fields are optional.
 *
 * @param assetType asset type, e.g. {@code real_estate}
 * @param active    only active (or inactive) assets
 * @param offset    pagination offset
 * @param limit     page size
 * @param fields    comma-separated sparse fieldset, e.g. {@code id,name,symbol}
 */
public record ListAssetsQuery(String assetType, Boolean active, Integer offset, Integer limit, String fields) {

    /** A query with no filters. */
    public static ListAssetsQuery all() {
        return new ListAssetsQuery(null, null, null, null, null);
    }

    String toQueryString() {
        return new QueryString()
            .add("asset_type", assetType)
            .add("active", active)
            .add("offset", offset)
            .add("limit", limit)
            .add("fields", fields)
            .toString();
    }
}
