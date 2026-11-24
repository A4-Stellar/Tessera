# API error budget (SLO burn rate)

Alerts from `infrastructure/monitoring/slo_rules.yml` (issue #312). The SLOs,
windows and thresholds are described in
[`infrastructure/monitoring/README.md`](../../infrastructure/monitoring/README.md).

**`TesseraApiErrorBudgetFastBurn`** (page). The budget will run out within
days if nothing changes.
- On the dashboard, check *Requests by status class* (availability) or
  *p99 latency by route* (latency) to find the failing or slow route.
- Check whether a deploy happened in the last hour; roll back if it lines
  up with the burn.
- For availability, check Soroban RPC health. Indexer refresh failures
  (`rwa_indexer_refresh_total{outcome="failure"}`) and RPC failures
  (`rwa_rpc_failures_total`) usually come before 5xx responses from
  snapshot-backed routes.
- For latency, check rate-limiter and Redis latency, and the database
  replica lag (`db_replica_lag_seconds`).

**`TesseraApiErrorBudgetSlowBurn`** (ticket). A sustained, lower-level burn
that will exhaust the budget before the window ends. Open an issue, find the
route responsible with the same panels, and fix it within the week.

**`TesseraApiErrorBudgetLow`** (ticket). Less than 25 % of the budget is
left. Freeze risky deploys and non-urgent migrations until it recovers.

**`TesseraApiErrorBudgetExhausted`** (page). The 30-day SLO is breached.
Only ship reliability fixes until the budget is positive again, and write a
post-incident review.

## Automated triage (P1)

`severity: page` alerts are P1. The triage bot
([automated-triage.md](automated-triage.md)) runs `check_postgres_connections`
and then `flush_redis_cache` (`cache:v1:*`, restored from a snapshot if the
flush fails), and posts the results in the alert's Slack thread. Rolling back a
deploy stays a human decision.

## Related Integrity Alerts

Integrity alerts tracked alongside SLOs in `slo_rules.yml`:
- **`ShadowDivergence`** (P1 page): Shadow verifier detected state discrepancy with primary indexer. See [indexer-lag.md](indexer-lag.md) and [shadow-divergence.md](shadow-divergence.md).
- **`ShadowLagHigh`** (P3 ticket): Shadow indexer is lagging behind primary on-chain ledger state. See [indexer-lag.md](indexer-lag.md).

