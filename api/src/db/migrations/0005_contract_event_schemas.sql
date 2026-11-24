-- Versioned Avro event schemas discovered from contract ABIs (event schema registry).
-- Each (contract, event) keeps one row per schema version so historical events
-- can be decoded with the schema that was current when they were emitted.
CREATE TABLE IF NOT EXISTS contract_event_schema (
    contract_id    TEXT        NOT NULL,
    event_name     TEXT        NOT NULL,
    schema_version BIGINT      NOT NULL CHECK (schema_version > 0),
    schema_json    TEXT        NOT NULL,
    compatibility  TEXT        NOT NULL,
    contract_hash  TEXT        NOT NULL,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (contract_id, event_name, schema_version)
);

-- Hash-change detection: "has this contract WASM already been registered?"
-- is answered by a single indexed lookup rather than a table scan.
CREATE INDEX IF NOT EXISTS idx_contract_event_schema_contract_hash
    ON contract_event_schema (contract_id, contract_hash);
