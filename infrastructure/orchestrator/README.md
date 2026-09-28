# tessera-orchestrator: cross-region database failover

Automated failover controller for Tessera's PostgreSQL streaming-replication
cluster (for example a primary in AWS `us-east-1` and standbys in `eu-west-1`).
The logic lives in [`src/failover.rs`](src/failover.rs).

## What it does

| Step | Behaviour |
|------|-----------|
| Monitor | Heartbeats the primary (`pg_is_in_recovery`, `pg_current_wal_lsn`, `pg_stat_replication`) and every standby (received/replayed LSN, replay delay, WAL receiver state) every `heartbeat_interval_ms`. Lag above `lag_alert_bytes` is logged as a warning. |
| Detect | The primary is declared dead after `failure_threshold` consecutive failed or read-only heartbeats. |
| Witness check | Standbys are re-probed. If any still streams from the primary, the controller is the partitioned side and refuses to fail over. A majority of standbys must be reachable. |
| Zero-data-loss gate | Only a synchronous standby is eligible (by default). It must hold all WAL any node is known to have and finish replaying it before promotion. |
| Fence | The old primary is switched to read-only and its client sessions terminated, when it is reachable. |
| Promote | `pg_promote()` on the candidate, confirmed by `pg_is_in_recovery() = false`, all within `promotion_deadline_ms` (30s) of detection. Otherwise the failover is aborted and nothing is re-routed. |
| Re-route | The database CNAME is switched via Route53 (`UPSERT`, SigV4-signed) or Cloudflare. A failed DNS update never rolls back the promotion; it is retried on every heartbeat until it succeeds. |

## Running

```sh
cp orchestrator.example.toml orchestrator.toml   # edit nodes and DNS
export TESSERA_DB_USE1_URL=postgres://orchestrator:...@use1-host/postgres
export TESSERA_DB_EUW1_URL=postgres://orchestrator:...@euw1-host/postgres
export AWS_ACCESS_KEY_ID=... AWS_SECRET_ACCESS_KEY=...   # or CLOUDFLARE_API_TOKEN
cargo run --release -- orchestrator.toml
```

Logs are JSON (`RUST_LOG` controls verbosity).

## Cluster requirements

- `synchronous_standby_names` on the primary must list the standby that should
  be promoted, and each standby's `application_name` must equal its `name` in
  the config. Without synchronous replication, zero data loss cannot be
  guaranteed; `require_synchronous_candidate = false` allows async promotion
  and `max_data_loss_bytes` bounds the acceptable gap.
- The monitoring role needs `pg_monitor`, `EXECUTE` on `pg_promote`, and
  superuser (or equivalent grants) for fencing.
- Standbys and the API should connect through the DNS name, not the node host,
  and use a low TTL (30 to 60s). Remaining standbys follow the new timeline
  (`recovery_target_timeline = 'latest'`, the default since PostgreSQL 12).
- Run the controller outside the primary's region, ideally one instance per
  region with only one active, so a regional outage does not take it down.
- The fenced former primary must be rebuilt (`pg_rewind` or re-clone) before it
  rejoins as a standby.
- Managed services that disallow `pg_promote` (RDS, Cloud SQL) need a
  `Promoter` implementation that calls the provider's promote API instead.

## Tests

```sh
cargo test
```

The controller tests run against an in-memory cluster on a paused tokio clock,
covering: normal lag reporting, transient failures, sync-standby promotion
with WAL catch-up and DNS switch inside 30s, controller partition, async-only
refusal, data-loss refusal, replay timeouts, slow promotion, and deferred DNS
retries. The Route53 signer is checked against the AWS SigV4 reference vector.
