package io.tessera.sdk.model;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;
import com.fasterxml.jackson.annotation.JsonProperty;

/**
 * A tokenized real-world asset indexed from Stellar.
 *
 * <p>Token amounts and cent values are strings to keep the API's {@code i128} precision;
 * use {@link java.math.BigInteger} for arithmetic.
 */
@JsonIgnoreProperties(ignoreUnknown = true)
public record Asset(
    @JsonProperty("id") long id,
    @JsonProperty("token_contract") String tokenContract,
    @JsonProperty("issuer") String issuer,
    @JsonProperty("name") String name,
    @JsonProperty("symbol") String symbol,
    @JsonProperty("asset_type") String assetType,
    @JsonProperty("description") String description,
    @JsonProperty("valuation_cents") String valuationCents,
    @JsonProperty("valuation_usd") double valuationUsd,
    @JsonProperty("decimals") int decimals,
    @JsonProperty("total_supply") String totalSupply,
    @JsonProperty("holders") int holders,
    @JsonProperty("active") boolean active,
    @JsonProperty("paused") boolean paused,
    @JsonProperty("compliance_contract") String complianceContract,
    @JsonProperty("created_at_ledger") long createdAtLedger,
    @JsonProperty("indexed_at_ledger") long indexedAtLedger,
    @JsonProperty("index_error") String indexError) {
}
