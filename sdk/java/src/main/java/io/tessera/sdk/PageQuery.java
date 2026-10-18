package io.tessera.sdk;

/**
 * Offset/limit pagination; either value may be {@code null}.
 *
 * @param offset pagination offset
 * @param limit  page size
 */
public record PageQuery(Integer offset, Integer limit) {

    /** First page of {@code limit} items. */
    public static PageQuery first(int limit) {
        return new PageQuery(null, limit);
    }

    String toQueryString() {
        return new QueryString().add("offset", offset).add("limit", limit).toString();
    }
}
