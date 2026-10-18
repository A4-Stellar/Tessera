package io.tessera.sdk.model;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;
import com.fasterxml.jackson.annotation.JsonProperty;
import com.fasterxml.jackson.databind.JsonNode;

/**
 * An indexed contract event, as returned by REST and pushed over the WebSocket. The shape of {@code data} depends on {@code eventType}.
 */
@JsonIgnoreProperties(ignoreUnknown = true)
public record TesseraEvent(
    @JsonProperty("id") long id,
    @JsonProperty("contract") String contract,
    @JsonProperty("event_type") String eventType,
    @JsonProperty("ledger") long ledger,
    @JsonProperty("timestamp") String timestamp,
    @JsonProperty("data") JsonNode data) {
}
