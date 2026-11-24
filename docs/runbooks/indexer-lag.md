# Indexer lag

Confirm the RPC endpoint is healthy, inspect the indexer error backlog, and compare the persisted ledger cursor with the latest network ledger. Scale or restart the poller only after preserving its cursor.

## Automated triage (P1)

The triage bot (`tessera-triage`, see [automated-triage.md](automated-triage.md))
runs these steps within 90 seconds of the alert and posts the results to Slack:

1. `check_postgres_connections`: reports whether the database is also
   saturated. It changes nothing.
2. `restart_indexer_pod`: rolling restart of `deployment/tessera-indexer`, then
   waits up to 60 s for the new pods to be ready. The ledger cursor is persisted
   in PostgreSQL, so a restart resumes from the last indexed ledger. If the new
   pods are not ready in time, the previous pod template is restored and the
   alert escalates.
3. `flush_redis_cache`: deletes `cache:v1:*` so snapshots built while the
   indexer was stuck stop being served. The keys are snapshotted first and
   restored if the flush fails.

If the bot reports `escalated`, continue with the manual steps above.
