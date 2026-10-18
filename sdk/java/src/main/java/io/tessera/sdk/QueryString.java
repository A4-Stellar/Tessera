package io.tessera.sdk;

import java.net.URLEncoder;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.List;

/** Order-preserving query-string builder that skips unset values. */
final class QueryString {
    private final List<String> pairs = new ArrayList<>();

    QueryString add(String key, Object value) {
        if (value == null) {
            return this;
        }
        String text = value.toString();
        if (!text.isEmpty()) {
            pairs.add(encode(key) + "=" + encode(text));
        }
        return this;
    }

    private static String encode(String value) {
        return URLEncoder.encode(value, StandardCharsets.UTF_8).replace("+", "%20");
    }

    @Override
    public String toString() {
        return pairs.isEmpty() ? "" : "?" + String.join("&", pairs);
    }
}
