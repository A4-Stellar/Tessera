-- Migration 0001: Core tables for Tessera API persistence layer (issue #42).
-- Designed for TimescaleDB; the `events` and `snapshot_history` tables are
-- converted to hypertables partitioned by `occurred_at` / `indexed_at`.

-- ── assets ─────────────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS assets (
    id                  BIGSERIAL PRIMARY KEY,
    token_contract      TEXT        NOT NULL UNIQUE,
    issuer              TEXT        NOT NULL,
    name                TEXT        NOT NULL,
    symbol              TEXT        NOT NULL,
    asset_type          TEXT        NOT NULL,
    description         TEXT        NOT NULL DEFAULT '',
    valuation_cents     BIGINT      NOT NULL DEFAULT 0,
    decimals            SMALLINT    NOT NULL DEFAULT 7,
    total_supply        NUMERIC(39) NOT NULL DEFAULT 0,
    active              BOOLEAN     NOT NULL DEFAULT TRUE,
    paused              BOOLEAN     NOT NULL DEFAULT FALSE,
    compliance_contract TEXT        NOT NULL,
    created_at_ledger   BIGINT      NOT NULL DEFAULT 0,
    indexed_at_ledger   BIGINT      NOT NULL DEFAULT 0,
    index_error         TEXT,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_assets_token_contract ON assets (token_contract);
CREATE INDEX IF NOT EXISTS idx_assets_active          ON assets (active);

-- ── holders ────────────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS holders (
    id            BIGSERIAL   PRIMARY KEY,
    asset_id      BIGINT      NOT NULL REFERENCES assets (id) ON DELETE CASCADE,
    address       TEXT        NOT NULL,
    balance       NUMERIC(39) NOT NULL DEFAULT 0,
    share_percent NUMERIC(8,4) NOT NULL DEFAULT 0,
    snapshot_ledger BIGINT    NOT NULL DEFAULT 0,
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (asset_id, address)
);

CREATE INDEX IF NOT EXISTS idx_holders_address  ON holders (address);
CREATE INDEX IF NOT EXISTS idx_holders_asset_id ON holders (asset_id);

-- ── transactions ───────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS transactions (
    id              BIGSERIAL   PRIMARY KEY,
    asset_id        BIGINT      NOT NULL REFERENCES assets (id) ON DELETE CASCADE,
    tx_hash         TEXT        NOT NULL,
    ledger_sequence BIGINT      NOT NULL,
    occurred_at     TIMESTAMPTZ NOT NULL,
    from_address    TEXT,
    to_address      TEXT,
    amount          NUMERIC(39),
    tx_type         TEXT        NOT NULL,   -- 'transfer' | 'mint' | 'burn' | 'clawback' | ...
    raw_data        JSONB
);

CREATE INDEX IF NOT EXISTS idx_transactions_asset_id        ON transactions (asset_id);
CREATE INDEX IF NOT EXISTS idx_transactions_ledger_sequence ON transactions (ledger_sequence);
CREATE INDEX IF NOT EXISTS idx_transactions_occurred_at     ON transactions (occurred_at DESC);
CREATE INDEX IF NOT EXISTS idx_transactions_from_address    ON transactions (from_address);
CREATE INDEX IF NOT EXISTS idx_transactions_to_address      ON transactions (to_address);

-- Convert to TimescaleDB hypertable partitioned by occurred_at.
-- (Requires TimescaleDB extension; silently skipped on plain PostgreSQL.)
DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM pg_extension WHERE extname = 'timescaledb'
    ) THEN
        PERFORM create_hypertable(
            'transactions',
            'occurred_at',
            chunk_time_interval => INTERVAL '7 days',
            if_not_exists => TRUE
        );
    END IF;
END;
$$;

-- ── events ─────────────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS events (
    id              BIGSERIAL   PRIMARY KEY,
    contract        TEXT        NOT NULL,
    event_type      TEXT        NOT NULL,
    ledger_sequence BIGINT      NOT NULL,
    occurred_at     TIMESTAMPTZ NOT NULL,
    data            JSONB       NOT NULL DEFAULT '{}'
);

CREATE INDEX IF NOT EXISTS idx_events_contract        ON events (contract);
CREATE INDEX IF NOT EXISTS idx_events_event_type      ON events (event_type);
CREATE INDEX IF NOT EXISTS idx_events_ledger_sequence ON events (ledger_sequence);
CREATE INDEX IF NOT EXISTS idx_events_occurred_at     ON events (occurred_at DESC);

DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM pg_extension WHERE extname = 'timescaledb'
    ) THEN
        PERFORM create_hypertable(
            'events',
            'occurred_at',
            chunk_time_interval => INTERVAL '7 days',
            if_not_exists => TRUE
        );
    END IF;
END;
$$;

-- ── snapshot_history ───────────────────────────────────────────────────────
-- Each row is one complete indexer snapshot. Used for historical balance
-- lookups (GET /v1/holders/:address/history) with sub-10ms query times
-- (covered by the composite index below).
CREATE TABLE IF NOT EXISTS snapshot_history (
    id              BIGSERIAL    PRIMARY KEY,
    asset_id        BIGINT       NOT NULL REFERENCES assets (id) ON DELETE CASCADE,
    address         TEXT         NOT NULL,
    balance         NUMERIC(39)  NOT NULL DEFAULT 0,
    ledger_sequence BIGINT       NOT NULL,
    indexed_at      TIMESTAMPTZ  NOT NULL DEFAULT NOW()
);

-- Composite index optimised for the history lookup query:
--   WHERE address = $1 [AND asset_id = $2] ORDER BY indexed_at DESC
CREATE INDEX IF NOT EXISTS idx_snapshot_history_address_time
    ON snapshot_history (address, indexed_at DESC);

CREATE INDEX IF NOT EXISTS idx_snapshot_history_asset_address_ledger
    ON snapshot_history (asset_id, address, ledger_sequence DESC);

DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM pg_extension WHERE extname = 'timescaledb'
    ) THEN
        PERFORM create_hypertable(
            'snapshot_history',
            'indexed_at',
            chunk_time_interval => INTERVAL '1 day',
            if_not_exists => TRUE
        );
    END IF;
END;
$$;
