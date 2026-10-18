package io.tessera.sdk.model;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;
import com.fasterxml.jackson.annotation.JsonInclude;
import com.fasterxml.jackson.annotation.JsonProperty;
import com.fasterxml.jackson.databind.JsonNode;

/**
 * Request body for {@code POST /v1/audit/entries}.
 */
@JsonIgnoreProperties(ignoreUnknown = true)
@JsonInclude(JsonInclude.Include.NON_NULL)
public record CreateAuditEntryRequest(
    @JsonProperty("action") JsonNode action,
    @JsonProperty("actor") String actor,
    @JsonProperty("target") String target,
    @JsonProperty("payload") JsonNode payload) {
}
