# Scaling pipeline operations

This document covers the first production cut of the ledger archive, Redis
cache, tenant API-key tiers, and event-table sharding work.

## Cold ledger archive

The archive planner lives in `api/src/indexer/archive.rs`.

- Keep the most recent `hot_retention_ledgers` in Postgres for low-latency API
  reads.
- Archive older rows in `batch_ledgers` JSONL/Zstd objects.
- Mark an archive range complete only after the object manifest has been
  committed.
- Resume from the persisted `archived_through` ledger; batches are contiguous,
  bounded, and idempotent.

Default key format:

```text
ledger-archive/{network}/{start_ledger}-{end_ledger}.jsonl.zst
```

## Redis response cache

The cache key policy lives in `api/src/cache.rs`.

- Keys are scoped by tenant and HTTP method.
- Path/query values are SHA-256 digested so sensitive query values are not
  embedded in Redis keys.
- Fresh TTLs include deterministic jitter to avoid synchronized expiry.
- Every response key has a corresponding `lock:{key}` stampede-control key.

Recommended defaults:

| Setting | Value |
| --- | ---: |
| Fresh TTL | 30 seconds |
| Stale TTL | 120 seconds |
| Lock TTL | 5 seconds |
| Jitter | 10% |

## Tenant API keys and rate tiers

Tenant identity helpers live in `api/src/middleware/tenant.rs`.

API keys use this shape:

```text
tenant_id.random_secret_at_least_24_chars
```

The raw secret is never used in rate-limit keys; the full API key is SHA-256
hashed first. Tier defaults:

| Tier | Requests | Window |
| --- | ---: | ---: |
| Free | 60 | 60s |
| Growth | 600 | 60s |
| Enterprise | 6000 | 60s |

## Event table sharding

Shard helpers live in `api/src/db/sharding.rs`.

- Use `event_shard(tenant_id, ledger_sequence, config)` to pick a table.
- Use `events_0000`, `events_0001`, ... table names.
- Create all shard tables during migration with `create_event_shard_sql`.
- Keep the base `events` table as the schema template for shard creation.

The default shard count is 32. Increase it only with a forward migration that
keeps old shards readable while new writes move to the expanded set.
