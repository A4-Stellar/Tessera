# Migrations — expand-and-contract workflow (issue #71)

Zero-downtime schema changes for the Tessera API. Each migration is a pair of
SQL files under `api/src/db/migrations/`:

```
0004_expand_thing.sql       # the change (additive, backward compatible)
0004_expand_thing.down.sql  # verified rollback script
```

The files are embedded into the binary with `include_str!` (see
`api/src/db/mod.rs`), so the distroless container can run them with no
filesystem access.

## Commands

```bash
# Apply all pending migrations (skips contract phases unless ALLOW_DESTRUCTIVE_MIGRATIONS=1)
DATABASE_URL=postgres://... cargo run -- migrate

# Show applied/pending state
DATABASE_URL=postgres://... cargo run -- migrate-status

# Roll back the newest migration; `N` rolls back N steps
DATABASE_URL=postgres://... cargo run -- migrate-down
DATABASE_URL=postgres://... cargo run -- migrate-down 2
```

## The rules

1. **Every migration ships a `.down.sql`.** The runner refuses to roll back a
   version without one. CI verifies each pair applies *and* reverts cleanly.
2. **Contract phases are gated.** A migration whose name contains `contract`
   is destructive (drops columns/tables/constraints). It is refused unless
   `ALLOW_DESTRUCTIVE_MIGRATIONS=1` is exported, which is only ever done by
   the operator after the new application version is fully rolled out.
3. **Expand phases run freely.** Additive changes (`ADD COLUMN`, new indexes,
   new tables) never need the flag because they are compatible with both the
   old and the new application version running simultaneously.
4. **Applied migrations are immutable.** The runner stores a SHA-256 of every
   applied file and fails if the file content changes afterwards. Fix forward
   with a new migration instead.
5. **Runs are serialized.** Each run takes `pg_advisory_lock(0x54455353)`
   before touching the schema, so two operators or a deploy racing a manual
   run cannot execute migrations concurrently.

## Expand-and-contract walkthrough

The reference implementation is the `0002`/`0003` pair (dual-write support for
holder balances). A real column-type change works like this:

```
Step 1  EXPAND    0002_expand_holder_balance_minor.sql
                  Add the new column nullable + indexes CONCURRENTLY.
                  Deploy the new application version that dual-writes
                  old and new columns. Old versions keep working untouched.

Step 2  BACKFILL  Populate the new column for pre-existing rows
                  (batched UPDATEs, or a one-off migration with the
                  -- transactional header). No exclusive locks.

Step 3  CONTRACT  0003_contract_holder_balance_minor.sql
                  After every replica runs the new version:
                  pre-flight guards verify the backfill completed and the
                  dual-write never diverged, then the old column is dropped
                  and the new one promoted. Run with
                  ALLOW_DESTRUCTIVE_MIGRATIONS=1.

Step 4  CLEANUP   Remove dual-write code from the application at leisure.
```

If Step 3 needs undoing before the next deploy, `migrate-down` runs
`0003_...down.sql`, which restores the expand-phase shape.

## Lock-safety checklist (for any new migration)

- [ ] `ADD COLUMN` has no default (or a constant default) — no rewrite.
- [ ] Indexes on non-trivial tables use `CREATE INDEX CONCURRENTLY` — which
      also means the file must **not** use the `-- transactional` header.
- [ ] Constraints are added `NOT VALID` + `VALIDATE CONSTRAINT` in separate
      statements when the table is large.
- [ ] Renames are always two-phase: add new → dual-write → drop old.
- [ ] No `ALTER TABLE ... SET NOT NULL` without a validated CHECK constraint
      backing it (PostgreSQL ≥ 12 skips the scan when one exists).
- [ ] The `.down.sql` restores the exact expand-phase shape and is idempotent.
