# Database pool exhaustion

Inspect long-running queries and pool wait time. Resolve leaked transactions before increasing the pool size, and verify the database connection limit first.

## Automated triage

`DatabasePoolExhaustion` is P2, so the bot does not act on it. When a P1 alert
runs this runbook, the bot only runs `check_postgres_connections` and reports
the connection count. It never terminates sessions or resizes the pool.
