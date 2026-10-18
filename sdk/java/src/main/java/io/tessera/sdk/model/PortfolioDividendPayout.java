package io.tessera.sdk.model;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;
import com.fasterxml.jackson.annotation.JsonProperty;

/**
 * A dividend payout attributed to a portfolio.
 *
 * <p>Token amounts and cent values are strings to keep the API's {@code i128} precision;
 * use {@link java.math.BigInteger} for arithmetic.
 */
@JsonIgnoreProperties(ignoreUnknown = true)
public record PortfolioDividendPayout(
    @JsonProperty("distribution_id") long distributionId,
    @JsonProperty("asset_id") long assetId,
    @JsonProperty("payment_token") String paymentToken,
    @JsonProperty("amount") String amount,
    @JsonProperty("amount_usd") double amountUsd,
    @JsonProperty("claimed_at_ledger") long claimedAtLedger,
    @JsonProperty("timestamp") String timestamp) {
}
