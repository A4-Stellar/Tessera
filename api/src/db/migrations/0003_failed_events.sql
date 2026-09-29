-- Migration 0003: Dead-letter quarantine for unparseable contract events
-- (issue #163). Mirrors the JSON DLQ store (`RWA_DLQ_STORE`); rows are
-- best-effort inserts from the indexer and the admin retry endpoint marks
-- recovered rows resolved.

CREATE TABLE IF NOT EXISTS failed_events (
    event_id        BIGINT      PRIMARY KEY,   -- FNV-1a of the RPC event id
    source_id       TEXT        NOT NULL,      -- RPC-level event id
    contract_id     TEXT        NOT NULL DEFAULT '',
    ledger_sequence BIGINT      NOT NULL,
    raw_payload     TEXT        NOT NULL DEFAULT '',
    decode_stage    TEXT        NOT NULL DEFAULT '',
    error           TEXT        NOT NULL DEFAULT '',
    quarantined_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    resolved        BOOLEAN     NOT NULL DEFAULT FALSE,
    retry_count     INTEGER     NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_failed_events_unresolved
    ON failed_events (quarantined_at DESC) WHERE NOT resolved;
CREATE INDEX IF NOT EXISTS idx_failed_events_contract
    ON failed_events (contract_id);
