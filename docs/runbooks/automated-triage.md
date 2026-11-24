# Automated P1 triage

The triage bot (`tessera-triage`, in `infrastructure/orchestrator`) runs the
automated part of a runbook as soon as a P1 alert fires, before a human is
paged. Issue #474.

## Flow

```text
Prometheus rule (labels.priority=P1, annotations.runbook_url=docs/runbooks/<slug>.md)
   │
   ▼
Alertmanager ── route priority="P1" ──► Grafana OnCall integration
                                            │ escalation chain "tessera-<env>-p1"
                                            ├─ 0  trigger_webhook ──► tessera-triage /webhook/oncall
                                            ├─ 1  notify(channel)        #tessera-incidents
                                            ├─ 2  wait 5 min
                                            ├─ 3  notify(on-call engineer)  from the schedule
                                            ├─ 4  wait 15 min
                                            └─ 5  notify(manager)
```

Acknowledging or resolving the alert group stops the chain. The chain is
defined in `infrastructure/terraform/modules/grafana-oncall`. The Alertmanager
route is in `infrastructure/monitoring/alertmanager.yaml`.

## What the bot does

The bot picks the steps from the runbook file name in `runbook_url`:

| runbook | steps |
|---|---|
| `indexer-lag` | `check_postgres_connections` → `restart_indexer_pod` → `flush_redis_cache` |
| `api-errors`, `api-error-budget` | `check_postgres_connections` → `flush_redis_cache` |
| `soroban-rpc` | `check_postgres_connections` → `restart_indexer_pod` |
| `database-pool`, anything else | `check_postgres_connections` |

| step | timeout | what it changes | rollback |
|---|---|---|---|
| `check_postgres_connections` | 5 s | nothing; reports `degraded` at ≥ 90 % of `max_connections` | not needed |
| `restart_indexer_pod` | 60 s | sets `kubectl.kubernetes.io/restartedAt` on `deployment/tessera-indexer` (a rolling restart) when a pod is stuck or the alert is indexer lag, then waits for the rollout | restores the previous annotation, which rolls back to the previous pods |
| `flush_redis_cache` | 10 s | `UNLINK`s keys matching `cache:v1:*`, never `FLUSHDB`; refuses above 10 000 keys | `RESTORE`s every key from the `DUMP` snapshot taken first |

The whole runbook has a 90 s budget. When a step fails or times out, the bot
rolls that step back, skips the remaining steps and marks the report
`escalated`. The escalation chain keeps running, so a human gets paged as
usual.

Only P1 alerts are acted on. A `priority` label wins; otherwise `severity`
`critical`/`page` counts as P1. Repeats of the same alert (by fingerprint) are
ignored for 10 minutes.

## Slack report

The bot posts one message per alert to the Slack webhook: a summary line per
step, followed by the structured list:

```json
[
  { "action": "check_postgres_connections", "outcome": "healthy", "duration_ms": 12, "next_step": "restart_indexer_pod" },
  { "action": "restart_indexer_pod", "outcome": "remediated", "duration_ms": 27004, "next_step": "flush_redis_cache" },
  { "action": "flush_redis_cache", "outcome": "remediated", "duration_ms": 31, "next_step": "monitor alert until it resolves" }
]
```

`outcome` is one of `healthy`, `degraded`, `remediated`, `skipped`, `failed`,
`timed_out`.

## Audit trail

Every step writes to the API's hash-chained audit log
(`POST /v1/audit/entries`, actor `tessera-triage-bot`, action type
`administrative_action`):

- `triage.intent.<step>` before the step runs,
- `triage.result.<step>` with the step report,
- `triage.rollback.<step>` when a rollback was attempted.

If the intent cannot be written, the step does not run and the alert
escalates. No action is taken without an audit record.

## Deploying

1. Build `infrastructure/orchestrator/Dockerfile.triage`.
2. Create the `tessera-triage` Secret with `webhook-token`, `database-url`
   (a role with `pg_monitor`), `redis-url`, `slack-webhook-url` and, if the API
   requires one, `audit-api-key`.
3. `kubectl apply -f infrastructure/k8s/triage-bot/`. The service account
   may only get and patch `deployment/tessera-indexer` and list pods.
4. Apply the `grafana-oncall` Terraform module with `triage_bot_url` pointing
   at the service's `/webhook/oncall`, and mount its
   `alertmanager_integration_url` output as
   `/etc/alertmanager/secrets/grafana-oncall-url`.

## Testing

- `cargo test` in `infrastructure/orchestrator` runs the chaos tests in
  `tests/triage_chaos.rs` against a simulated cluster. They cover: a stalled
  indexer restarted within 90 s, a replacement pod that never becomes ready
  (rolled back and escalated), a failed cache flush (restored), the audit
  trail being down (no action taken), and repeated, resolved or P2 alerts
  (ignored).
- On a real cluster, `tests/chaos/triage_p1_check.sh` partitions the indexer
  from Soroban RPC with Chaos Mesh, fires the P1 alert and checks that the
  indexer has been restarted and is ready within 90 s.
- `python3 infrastructure/monitoring/tests/check_runbooks.py` checks that
  every alert links to an existing file in `docs/runbooks/`.
