#!/usr/bin/env bash
# Chaos test for issue #474: trigger a P1 alert, verify the triage bot takes
# corrective action (a completed rolling restart of the indexer) within 90 s.
#
# Needs kubectl access to the cluster and the Alertmanager API:
#   ALERTMANAGER_URL=http://alertmanager.monitoring:9093 tests/chaos/triage_p1_check.sh
set -euo pipefail

NAMESPACE="${NAMESPACE:-tessera}"
DEPLOYMENT="${DEPLOYMENT:-tessera-indexer}"
ALERTMANAGER_URL="${ALERTMANAGER_URL:?set ALERTMANAGER_URL}"
BUDGET_SECONDS="${BUDGET_SECONDS:-90}"
here="$(cd "$(dirname "$0")" && pwd)"

annotation() {
  kubectl -n "$NAMESPACE" get deployment "$DEPLOYMENT" \
    -o jsonpath='{.spec.template.metadata.annotations.kubectl\.kubernetes\.io/restartedAt}'
}

before="$(annotation || true)"
kubectl apply -f "$here/triage_p1_experiment.yaml"
trap 'kubectl delete -f "$here/triage_p1_experiment.yaml" --ignore-not-found' EXIT

# Fire the alert directly so the test does not wait out the rule's `for: 5m`.
now="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
curl -fsS -X POST "$ALERTMANAGER_URL/api/v2/alerts" -H 'Content-Type: application/json' -d "[{
  \"labels\": {\"alertname\": \"HighIndexerLag\", \"severity\": \"critical\", \"priority\": \"P1\", \"chaos\": \"triage-p1-$now\"},
  \"annotations\": {\"summary\": \"chaos: indexer partitioned from RPC\",
    \"runbook_url\": \"https://github.com/A4-Stellar/Tessera/blob/main/docs/runbooks/indexer-lag.md\"},
  \"startsAt\": \"$now\"
}]"
fired=$(date +%s)

while :; do
  elapsed=$(( $(date +%s) - fired ))
  current="$(annotation || true)"
  if [[ -n "$current" && "$current" != "$before" ]] \
    && kubectl -n "$NAMESPACE" rollout status "deployment/$DEPLOYMENT" --timeout=1s >/dev/null 2>&1; then
    echo "PASS: indexer restarted (restartedAt=$current) and ready ${elapsed}s after the P1 alert"
    exit 0
  fi
  if (( elapsed > BUDGET_SECONDS )); then
    echo "FAIL: no completed indexer restart within ${BUDGET_SECONDS}s (restartedAt still '${current}')" >&2
    exit 1
  fi
  sleep 3
done
