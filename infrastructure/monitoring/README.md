# API SLOs & error budgets

Service Level Objectives for the Tessera REST API (issue #312): Prometheus
recording and alerting rules, Alertmanager routing to PagerDuty and Slack,
and a Grafana dashboard.

| File | Purpose |
|------|---------|
| `slo_rules.yml` | SLI recording rules, error-budget and burn-rate series, alerts |
| `alertmanager.yml` | Routes `page` alerts to PagerDuty + Slack, `ticket` alerts to Slack |
| `slo_dashboard.json` | Grafana dashboard (uid `tessera-api-slo`) |
| `tests/slo_rules_test.yml` | `promtool` unit tests for the rules |
| `tests/check_dashboard.py` | Checks the dashboard only queries recorded series |

## SLOs

Both SLIs come from metrics the API already exports at `GET /metrics`
(`rwa_http_requests_total`, `rwa_http_request_duration_seconds`), excluding
the `/health` and `/metrics` probe routes.

| SLO | SLI (good event) | Objective | 30-day error budget |
|-----|------------------|-----------|---------------------|
| Availability | response status is not 5xx | ≥ 99.9 % | 0.1 % of requests (~43 min of full outage) |
| Latency | response served in ≤ 100 ms | ≥ 99 % (p99 < 100 ms) | 1 % of requests |

The latency SLI counts requests in the `le="0.1"` bucket, so the API exports
`rwa_http_request_duration_seconds` as a histogram with a 100 ms bucket
(`HTTP_LATENCY_BUCKETS` in `api/src/main.rs`). Don't remove that bucket.

## Burn-rate alerts

The alerts use the multi-window, multi-burn-rate scheme from the Google SRE
Workbook. A burn rate of 1x spends the budget exactly over 30 days. Each
alert needs both its long window (to detect the burn) and its short window
(so it clears quickly once the burn stops).

| Alert | Severity | Burn rate | Long / short window | Budget spent when it fires |
|-------|----------|-----------|---------------------|----------------------------|
| `TesseraApiErrorBudgetFastBurn` | page | > 14.4x | 1h / 5m | 2 % |
| `TesseraApiErrorBudgetFastBurn` | page | > 6x | 6h / 30m | 5 % |
| `TesseraApiErrorBudgetSlowBurn` | ticket | > 3x | 1d / 2h | 10 % |
| `TesseraApiErrorBudgetSlowBurn` | ticket | > 1x | 3d / 6h | 10 % |
| `TesseraApiErrorBudgetLow` | ticket | — | budget < 25 % remaining | — |
| `TesseraApiErrorBudgetExhausted` | page | — | budget ≤ 0 | 100 % |

Every alert carries a `slo` label (`availability` or `latency`).
Alertmanager inhibits burn and low-budget alerts while the same SLO's budget
is exhausted, and inhibits tickets while a page for that SLO is firing.

## Deploy

1. **Prometheus**: add the rules file and point alerts at Alertmanager:

   ```yaml
   rule_files:
     - /etc/prometheus/rules/slo_rules.yml
   alerting:
     alertmanagers:
       - static_configs:
           - targets: ["alertmanager:9093"]
   ```

   The 30-day windows need at least 30 days of retention
   (`--storage.tsdb.retention.time=35d`). Until then the 30-day series cover
   only the data that exists.

2. **Alertmanager**: use `alertmanager.yml` and mount the two secrets it
   reads as files:
   - `/etc/alertmanager/secrets/pagerduty_routing_key`: PagerDuty Events v2
     integration key.
   - `/etc/alertmanager/secrets/slack_webhook_url`: Slack incoming webhook
     for `#tessera-alerts`.

3. **Grafana**: import `slo_dashboard.json` (Dashboards → Import), or
   provision it from a dashboards provider directory. Pick the Prometheus
   data source in the `Prometheus` variable. The dashboard also includes a
   quarantined dead-letter event panel sourced from
   `tessera_dlq_quarantined_total`.

## Validate

```bash
cd infrastructure/monitoring
docker run --rm -v "$PWD:/w" -w /w --entrypoint promtool prom/prometheus:v2.54.1 check rules slo_rules.yml
docker run --rm -v "$PWD:/w" -w /w --entrypoint promtool prom/prometheus:v2.54.1 test rules tests/slo_rules_test.yml
docker run --rm -v "$PWD:/w" -w /w --entrypoint amtool prom/alertmanager:v0.27.0 check-config alertmanager.yml
python3 tests/check_dashboard.py
python3 tests/check_runbooks.py
```

## Runbook

The burn-rate runbook lives in
[`docs/runbooks/api-error-budget.md`](../../docs/runbooks/api-error-budget.md),
which every alert's `runbook_url` annotation links to. All alerts link to a
file under `docs/runbooks/`; `python3 tests/check_runbooks.py` checks this.
