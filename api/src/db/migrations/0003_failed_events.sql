-- Migration 0003: Dead-letter queue for unparseable contract events (issue #163).
-- An event whose XDR cannot be decoded is quarantined here instead of halting
-- ingestion. The payload is stored exactly as Soroban RPC `getEvents` returned
-- it (base64 `ScVal` XDR), so `POST /v1/admin/dlq/retry` can re-decode it
-- after a parser fix. Applied idempotently at startup by `indexer::dlq`.

CREATE TABLE IF NOT EXISTS failed_events (
    id               BIGSERIAL   PRIMARY KEY,
    event_id         TEXT        NOT NULL UNIQUE,   -- RPC event id (TOID + event index)
    contract_id      TEXT,                          -- C... strkey; absent for system events
    ledger_sequence  BIGINT      NOT NULL,
    ledger_closed_at TEXT,                          -- ISO-8601, as returned by RPC
    topic_xdr        TEXT[]      NOT NULL,          -- base64 ScVal per topic
    value_xdr        TEXT        NOT NULL,          -- base64 ScVal event data
    error            TEXT        NOT NULL,          -- most recent decode error
    status           TEXT        NOT NULL DEFAULT 'quarantined'
                                 CHECK (status IN ('quarantined', 'resolved')),
    attempts         INTEGER     NOT NULL DEFAULT 1,
    first_failed_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_failed_at   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    resolved_at      TIMESTAMPTZ
);

-- Retry scans only the (small) quarantined subset, oldest first.
CREATE INDEX IF NOT EXISTS idx_failed_events_quarantined
    ON failed_events (id) WHERE status = 'quarantined';

CREATE INDEX IF NOT EXISTS idx_failed_events_contract_ledger
    ON failed_events (contract_id, ledger_sequence);
