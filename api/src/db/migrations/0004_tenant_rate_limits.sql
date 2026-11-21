-- Per-tenant token-bucket configuration (issue #376).
CREATE TABLE IF NOT EXISTS tenant_rate_limits (
    tenant_id               TEXT        PRIMARY KEY,
    capacity                BIGINT      NOT NULL CHECK (capacity > 0),
    refill_rate             BIGINT      NOT NULL CHECK (refill_rate > 0),
    refill_interval_seconds BIGINT      NOT NULL CHECK (
        refill_interval_seconds > 0 AND refill_interval_seconds <= 86400
    ),
    updated_at              TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
