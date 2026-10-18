package io.tessera.sdk.model;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;
import com.fasterxml.jackson.annotation.JsonProperty;

/**
 * A dividend distribution pool.
 *
 * <p>Token amounts and cent values are strings to keep the API's {@code i128} precision;
 * use {@link java.math.BigInteger} for arithmetic.
 */
@JsonIgnoreProperties(ignoreUnknown = true)
public record Distribution(
    @JsonProperty("id") long id,
    @JsonProperty("asset_token") String assetToken,
    @JsonProperty("payment_token") String paymentToken,
    @JsonProperty("total_amount") String totalAmount,
    @JsonProperty("distributed") String distributed,
    @JsonProperty("claimed_percent") double claimedPercent,
    @JsonProperty("overflow_detected") boolean overflowDetected,
    @JsonProperty("completed") boolean completed,
    @JsonProperty("created_at_ledger") long createdAtLedger,
    @JsonProperty("fiat_equivalent_usd") Double fiatEquivalentUsd) {
}
