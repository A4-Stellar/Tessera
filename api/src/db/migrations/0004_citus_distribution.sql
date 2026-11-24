-- Migration 0004: Citus Horizontal Sharding of Analytics and Events Tables (Issue #43 / #424).
-- Safely distributes analytics and event tables online on `asset_id` as the distribution column.

-- 1. Enable Citus extension if available in the PostgreSQL environment.
CREATE EXTENSION IF NOT EXISTS citus;

-- 2. Ensure `asset_id` exists on `events` table for horizontal partitioning.
ALTER TABLE events ADD COLUMN IF NOT EXISTS asset_id BIGINT;
CREATE INDEX IF NOT EXISTS idx_events_asset_id ON events (asset_id);

-- 3. Analytics table for real-time and aggregate metrics.
CREATE TABLE IF NOT EXISTS analytics (
    id                  BIGSERIAL,
    asset_id            BIGINT NOT NULL,
    metric_name         TEXT NOT NULL,
    metric_value        NUMERIC(39, 4) NOT NULL DEFAULT 0,
    tvl_cents           BIGINT NOT NULL DEFAULT 0,
    volume_24h_cents    BIGINT NOT NULL DEFAULT 0,
    active_holders      INT NOT NULL DEFAULT 0,
    recorded_at         TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (asset_id, id)
);

CREATE INDEX IF NOT EXISTS idx_analytics_asset_time ON analytics (asset_id, recorded_at DESC);
CREATE INDEX IF NOT EXISTS idx_analytics_metric_name ON analytics (metric_name, recorded_at DESC);

-- 4. Safely distribute tables online using Citus `create_distributed_table`.
-- When Citus is present, partition tables by hash on `asset_id` and co-locate
-- them to ensure local joins on worker shards.
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'citus') THEN
        -- Safely distribute `events` table online
        IF NOT EXISTS (
            SELECT 1 FROM pg_dist_partition 
            WHERE logicalrelid = 'events'::regclass
        ) THEN
            PERFORM create_distributed_table(
                'events',
                'asset_id',
                'hash'
            );
        END IF;

        -- Safely distribute `analytics` table online, co-located with `events`
        IF NOT EXISTS (
            SELECT 1 FROM pg_dist_partition 
            WHERE logicalrelid = 'analytics'::regclass
        ) THEN
            PERFORM create_distributed_table(
                'analytics',
                'asset_id',
                'hash',
                colocate_with => 'events'
            );
        END IF;

        -- Safely distribute `asset_metric_windows` table online, co-located with `events`
        IF EXISTS (SELECT 1 FROM pg_class WHERE relname = 'asset_metric_windows') THEN
            IF NOT EXISTS (
                SELECT 1 FROM pg_dist_partition 
                WHERE logicalrelid = 'asset_metric_windows'::regclass
            ) THEN
                PERFORM create_distributed_table(
                    'asset_metric_windows',
                    'asset_id',
                    'hash',
                    colocate_with => 'events'
                );
            END IF;
        END IF;
    END IF;
END;
$$;
