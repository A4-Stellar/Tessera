package io.tessera.sdk;

/**
 * Window controls for {@code GET /v1/assets/{id}/metrics/analytics}.
 *
 * @param window     window length, e.g. {@code 1h} or {@code 24h}
 * @param windowType {@code sliding} or {@code tumbling}
 * @param limit      maximum number of windows
 */
public record AnalyticsWindowQuery(String window, String windowType, Integer limit) {

    String toQueryString() {
        return new QueryString()
            .add("window", window)
            .add("window_type", windowType)
            .add("limit", limit)
            .toString();
    }
}
