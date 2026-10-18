package io.tessera.sdk.model;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;
import com.fasterxml.jackson.annotation.JsonProperty;
import com.fasterxml.jackson.databind.JsonNode;
import java.util.List;

/**
 * A structured Soroban diagnostic event.
 */
@JsonIgnoreProperties(ignoreUnknown = true)
public record DiagnosticEvent(
    @JsonProperty("contract") String contract,
    @JsonProperty("event_type") String eventType,
    @JsonProperty("topics") List<JsonNode> topics,
    @JsonProperty("data") JsonNode data,
    @JsonProperty("in_successful_contract_call") boolean inSuccessfulContractCall,
    @JsonProperty("error_code") Long errorCode) {

    /** Normalises absent collections to empty, immutable lists. */
    public DiagnosticEvent {
        topics = topics == null ? List.of() : List.copyOf(topics);
    }
}
