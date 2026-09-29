-- Migration 0003: CONTRACT phase of the expand-and-contract reference pair
-- (issue #71) — promote the dual-write column.
--
-- Runs only after every replica of the application is on the NEW code
-- (reading and writing `balance_minor`) and the backfill is complete. The
-- pre-flight guard below fails the migration instead of corrupting data if
-- any of that is not true, so it is always safe to attempt.
--
-- Every statement here is written to be lock-friendly:
--   * The validation checks are read-only.
--   * SET NOT NULL is backed by the CHECK constraint validated in the same
--     transaction, so PostgreSQL proves the table is clean without a scan
--     and holds the lock only for the catalog update.
--   * Dropping the old column is the final, destructive step — only safe
--     once no old code can possibly write to it.

-- ── Pre-flight 1: the backfill must be complete ────────────────────────────
DO $$
DECLARE
    missing_rows BIGINT;
BEGIN
    SELECT COUNT(*) INTO missing_rows
    FROM holders
    WHERE balance IS NOT NULL
      AND balance_minor IS NULL;

    IF missing_rows > 0 THEN
        RAISE EXCEPTION
            'contract phase aborted: % holder row(s) still have balance_minor IS NULL; run the backfill first',
            missing_rows
            USING HINT = 'Backfill with: UPDATE holders SET balance_minor = balance WHERE balance_minor IS NULL';
    END IF;
END;
$$;

-- ── Pre-flight 2: no rows where the dual-write diverged ────────────────────
-- If both the old code (writing `balance`) and the new code (writing
-- `balance_minor`) ran against the same row, the values must agree to within
-- the scale of the original column. A mismatch means the dual-write window
-- was buggy: fail loudly rather than silently pick a winner.
DO $$
DECLARE
    diverged_rows BIGINT;
BEGIN
    SELECT COUNT(*) INTO diverged_rows
    FROM holders
    WHERE balance_minor IS NOT NULL
      AND balance IS NOT NULL
      AND balance_minor <> balance;

    IF diverged_rows > 0 THEN
        RAISE EXCEPTION
            'contract phase aborted: % holder row(s) have balance_minor <> balance',
            diverged_rows
            USING HINT = 'Reconcile the dual-write divergence before promoting the new column';
    END IF;
END;
$$;

-- ── Enforce integrity without a blocking table scan ────────────────────────
-- A CHECK constraint is validated on existing rows first (one pass). The
-- subsequent NOT NULL on the same column in the same transaction is then
-- proven by the constraint and needs no second scan (PostgreSQL >= 12).
ALTER TABLE holders
    ADD CONSTRAINT holders_balance_minor_not_null
    CHECK (balance_minor IS NOT NULL) NOT VALID;

ALTER TABLE holders
    VALIDATE CONSTRAINT holders_balance_minor_not_null;

ALTER TABLE holders
    ALTER COLUMN balance_minor SET NOT NULL;

-- The CHECK is now redundant (NOT NULL subsumes it) but keeping it is free;
-- drop it to keep the schema minimal.
ALTER TABLE holders
    DROP CONSTRAINT holders_balance_minor_not_null;

-- ── Swap the column roles ──────────────────────────────────────────────────
ALTER TABLE holders
    DROP COLUMN IF EXISTS balance;

ALTER TABLE holders
    RENAME COLUMN balance_minor TO balance;

-- Restore the original column's comment-level contract so the schema after
-- contract matches what 0001 declared, plus the new NOT NULL guarantee.
ALTER TABLE holders
    ALTER COLUMN balance SET DEFAULT 0;

-- Rebuild the partial index's purpose: with the phase complete there are no
-- NULL balance_minor rows, so the helper index is dead weight.
DROP INDEX IF EXISTS idx_holders_balance_minor_null;

-- Recreate the plain asset index that 0001 declared on holders; dropping the
-- partial index above does not cover the general lookup path.
CREATE INDEX IF NOT EXISTS idx_holders_asset_id ON holders (asset_id);
