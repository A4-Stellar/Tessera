-- Migration 0002: EXPAND phase of the expand-and-contract reference pair
-- (issue #71) — dual-write support for holder balances.
--
-- The old and the new application version run side by side during a rolling
-- deploy. To keep both writing consistent data without locks:
--
--   * The NEW code reads `holders.balance` (NUMERIC, exact) as before.
--   * The OLD code knows nothing about `balance_minor`.
--
-- Adding a nullable column takes no table-wide lock beyond a brief
-- ACCESS EXCLUSIVE metadata grab (a catalog-only change in modern
-- PostgreSQL), so this phase never blocks concurrent readers or writers.
-- A NOT NULL constraint is deliberately deferred to the contract phase
-- (0003), because it would require a full-table scan and a long lock.
--
-- Backfill note: the column starts as NULL for pre-existing rows. The
-- application (or the contract phase, pre-flight) backfills it with
-- `balance_minor = (balance * 10^decimals)::numeric` before 0003 promotes it.

-- ── holders: add nullable dual-write column ────────────────────────────────
ALTER TABLE holders
    ADD COLUMN IF NOT EXISTS balance_minor NUMERIC(39);

-- Plain ADD COLUMN with no default is a catalog-only change on modern
-- PostgreSQL: no table rewrite, no prolonged lock. The NOT NULL constraint
-- is deliberately deferred to the contract phase (0003), where it is backed
-- by a pre-validated CHECK constraint instead of a blocking scan.

-- Index to support the contract phase's validation scan
-- (finding rows where balance_minor IS NULL while balance IS NOT NULL),
-- built CONCURRENTLY so it never blocks live writes.
CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_holders_balance_minor_null
    ON holders (asset_id)
    WHERE balance_minor IS NULL;

-- Partial indexes cannot be created inside a transaction block when
-- CONCURRENTLY is used; this engine executes statements in autocommit,
-- which is exactly what CONCURRENTLY needs.
