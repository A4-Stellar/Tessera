package io.tessera.sdk;

import java.time.OffsetDateTime;
import java.time.format.DateTimeFormatter;

/**
 * Optional window for portfolio performance.
 *
 * @param startDate    start of the evaluation window (RFC 3339)
 * @param endDate      end of the evaluation window (RFC 3339)
 * @param discountRate discount rate used for NPV, e.g. {@code 0.05}
 */
public record PortfolioQuery(OffsetDateTime startDate, OffsetDateTime endDate, Double discountRate) {

    String toQueryString() {
        return new QueryString()
            .add("start_date", startDate == null ? null : DateTimeFormatter.ISO_OFFSET_DATE_TIME.format(startDate))
            .add("end_date", endDate == null ? null : DateTimeFormatter.ISO_OFFSET_DATE_TIME.format(endDate))
            .add("discount_rate", discountRate)
            .toString();
    }
}
