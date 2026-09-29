-- Down migration for 0003 (CONTRACT phase rollback).
--
-- Un-promotes `balance` back to the expand-phase state: nullable, with the
-- pre-contract name. This cannot restore data that the contract phase
-- destroyed (the old `balance` column was dropped), so it recreates the
-- column from the promoted one — the values are identical, which is exactly
-- what the contract phase's pre-flight guards guaranteed.
--
-- Like 0003 itself, this is written to be idempotent: running it on a
-- database that never reached the contract phase is a no-op.

-- The expand-phase column shape: nullable, no default.
ALTER TABLE holders
    ADD COLUMN IF NOT EXISTS balance_old NUMERIC(39);

-- Copy the promoted values back under the old name. The promoted column is
-- NOT NULL, so the copy is total; the "restored" column is therefore only
-- nullable in shape, not in content — matching the expand phase's end state
-- after the backfill.
UPDATE holders
SET    balance_old = balance
WHERE  balance_old IS NULL;

-- Atomic swap of the column names.
ALTER TABLE holders
    RENAME COLUMN balance TO balance_minor_promoted;

ALTER TABLE holders
    RENAME COLUMN balance_old TO balance;

ALTER TABLE holders
    RENAME COLUMN balance_minor_promoted TO balance_minor;

-- Promoted column keeps NOT NULL + DEFAULT 0; the restored `balance` is
-- nullable again (expand-phase shape).
ALTER TABLE holders
    ALTER COLUMN balance_minor SET NOT NULL;

ALTER TABLE holders
    ALTER COLUMN balance_minor SET DEFAULT 0;

-- Indexes on `balance`-related columns were dropped with the old column in
-- 0003; nothing here referenced `balance` by index. The asset_id index is
-- recreated by 0002.down / 0003 if needed; make sure it exists.
CREATE INDEX IF NOT EXISTS idx_holders_asset_id ON holders (asset_id);
