# Disaster recovery

Tessera keeps a warm standby in `us-west-2` with asynchronous pgBackRest WAL
shipping to S3 and GCS. The primary remains in `us-east-1`. A Cloudflare Worker
checks the primary every 30 seconds and can shift DNS weight to the standby when
the primary fails its health checks.

## Objectives

- **RTO** (time to restore service): **under 15 minutes**.
- **RPO** (maximum acceptable data loss): **under 5 minutes**.

These targets are enforced by the weekly automated drill and reported to
PagerDuty. If either target is missed, treat the DR posture as degraded.

## Architecture

- **Backups**: `infrastructure/db/pgbackrest.conf` defines two repositories
  (S3 and GCS) and `repo1-retention-full=7`. WAL segments are archived
  continuously by `infrastructure/db/pgbackrest-wal-archive.sh`; the script
  reports the current replication lag and fails when it exceeds five minutes.
- **Warm standby**: the Terraform module in
  `infrastructure/terraform/modules/aws/dr-standby` provisions an ECS Fargate
  service behind an internal ALB. It is wired into the `prod` environment via
  `enable_dr` and runs read-only against the replica. It is scaled to zero by
  default and is scaled up during a failover or a drill.
- **Traffic failover**: `infrastructure/cloudflare/failover-worker.js` performs
  a 30-second health check, stores the active region in Workers KV, and changes
  DNS weights accordingly. `wrangler.failover.toml` holds its bindings.
- **Automation**: `.github/workflows/dr_drill.yml` runs
  `infrastructure/dr/restore_verification.sh --drill` every Sunday at 03:17 UTC
  and on demand. `infrastructure/dr/dr_drill.sh` restores the latest WAL backup,
  scales the standby, runs API smoke tests, measures RTO/RPO, and emits a
  PagerDuty event.

## Failover procedure

1. Confirm the primary is unhealthy (API health endpoint, database connections,
   and recent alert context). Do not fail over for a transient blip.
2. Enable Cloudflare maintenance mode if the primary may still accept writes,
   to avoid a split brain while traffic moves.
3. Trigger failover by setting the `tessera-dr-active-region` key in the
   failover Worker's KV namespace to `us-west-2`, or let the Worker do it after
   three consecutive failed health checks. Record the exact time.
4. Scale the warm standby service up:
   ```sh
   aws ecs update-service \
     --cluster tessera-prod-dr \
     --service tessera-api-dr \
     --desired-count 2
   ```
5. Verify the replica has caught up: the RPO check in
   `infrastructure/db/pgbackrest-wal-archive.sh` must report under five minutes
   of lag. Promote the replica only after the last committed transaction is
   present.
6. Run the API smoke tests from `infrastructure/dr/dr_drill.sh` against
   `DR_API_URL`. Confirm settlement reads and writes succeed on the standby.
7. Report the measured RTO and RPO in the incident channel and the PagerDuty
   incident. Escalate if either exceeds the objective.

## Failback procedure

1. Restore the primary database from the newest pgBackRest full backup plus WAL
   on the primary cluster, then let it catch up from the standby.
2. Return the standby to read-only, scale it back down to zero, and re-enable
   WAL shipping from the primary.
3. Set the failover Worker's KV active-region key back to `us-east-1` and watch
   health checks for two minutes before closing the incident.

## Weekly drill

The scheduled workflow performs the drill end to end:

1. Restores the latest pgBackRest backup and WAL into an isolated namespace.
2. Scales the warm standby and points the DR API service at the restored
   database.
3. Runs the API smoke tests and records the restore time (RTO) and the age of
   the newest restored WAL segment (RPO).
4. Sends a `resolve`/`trigger` PagerDuty event with the measured values and
   uploads the logs as a workflow artifact.

To run it manually, dispatch the **Weekly disaster recovery drill** workflow,
or execute locally with the environment variables documented in
`.github/workflows/dr_drill.yml`:

```sh
./infrastructure/dr/restore_verification.sh --drill
```

## Interpreting failures

- **RPO above five minutes**: check WAL archiving on the primary, S3/GCS
  connectivity, and the `pgbackrest` stanza name. Re-run the archive health
  check before scheduling the next drill.
- **RTO above 15 minutes**: check standby image pull time, ECS capacity, and
  database restore throughput. Pre-warm the standby task definition if needed.
- **API tests fail after restore**: compare the replica's LSN with the primary
  and inspect the API deployment logs in the DR namespace.
