-- Migration 0002: Real-time event stream processing metric windows (Issue #96).
-- Persists tumbling and sliding window analytics over Soroban ledger events.

CREATE TABLE IF NOT EXISTS asset_metric_windows (
    id                          BIGSERIAL PRIMARY KEY,
    asset_id                    BIGINT NOT NULL REFERENCES assets (id) ON DELETE CASCADE,
    window_type                 TEXT NOT NULL,              -- 'tumbling' | 'sliding'
    window_duration             TEXT NOT NULL,              -- '1h' | '24h' | '7d' | '30d'
    window_duration_seconds     BIGINT NOT NULL,
    window_start                TIMESTAMPTZ NOT NULL,
    window_end                  TIMESTAMPTZ NOT NULL,
    trading_volume              NUMERIC(39) NOT NULL DEFAULT 0,
    trading_volume_usd          NUMERIC(18,4) NOT NULL DEFAULT 0,
    trade_count                 BIGINT NOT NULL DEFAULT 0,
    unique_active_traders       BIGINT NOT NULL DEFAULT 0,
    hourly_holder_growth_rate   NUMERIC(10,4) NOT NULL DEFAULT 0,
    volatility_index            NUMERIC(12,6) NOT NULL DEFAULT 0,
    moving_average_price_usd    NUMERIC(18,6) NOT NULL DEFAULT 0,
    open_price_usd              NUMERIC(18,6) NOT NULL DEFAULT 0,
    high_price_usd              NUMERIC(18,6) NOT NULL DEFAULT 0,
    low_price_usd               NUMERIC(18,6) NOT NULL DEFAULT 0,
    close_price_usd             NUMERIC(18,6) NOT NULL DEFAULT 0,
    created_at                  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_asset_metric_windows_asset_time
    ON asset_metric_windows (asset_id, window_type, window_start DESC);

CREATE INDEX IF NOT EXISTS idx_asset_metric_windows_end_time
    ON asset_metric_windows (window_end DESC);

-- Convert to TimescaleDB hypertable partitioned by window_start.
DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM pg_extension WHERE extname = 'timescaledb'
    ) THEN
        PERFORM create_hypertable(
            'asset_metric_windows',
            'window_start',
            chunk_time_interval => INTERVAL '1 day',
            if_not_exists => TRUE
        );
    END IF;
END;
$$;
