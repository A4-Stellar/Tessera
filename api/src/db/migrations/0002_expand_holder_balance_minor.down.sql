-- Down migration for 0002 (EXPAND phase rollback).
--
-- Drops the dual-write column added by the expand phase. Safe to run while
-- the new application version is still deployed ONLY IF that version has
-- already been stopped or reverted: after this runs, the new code's writes
-- to `holders.balance_minor` would fail. Rollback order for a live system
-- is therefore:
--   1. Roll the application back to the old version (reads/writes balance).
--   2. Run this down migration.
--   3. Continue with the old version until the next deploy.

DROP INDEX IF EXISTS idx_holders_balance_minor_null;

ALTER TABLE holders
    DROP COLUMN IF EXISTS balance_minor;
