# API errors

Check the API logs by route and dependency, then rollback the newest deployment if the error rate began immediately after a release. Do not disable the alert without recording the incident.

## Automated triage (P1)

The triage bot ([automated-triage.md](automated-triage.md)) runs
`check_postgres_connections` and then `flush_redis_cache` (`cache:v1:*`,
restored from a snapshot if the flush fails). It never rolls back a deployment:
that decision stays with the on-call engineer.
