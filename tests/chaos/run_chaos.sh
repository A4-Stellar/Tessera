#!/usr/bin/env bash
# Run every experiment in chaos_suite.yaml and write a resilience scorecard
# (issue #314). Requires kubectl access to a cluster with Chaos Mesh, curl, jq.
#
#   API_URL=https://api.example.test ./run_chaos.sh
#
# Env: API_URL (required), CHAOS_DURATION_SECS (60, must match the manifests),
#      PROBE_INTERVAL_SECS (2), MIN_AVAILABILITY_PERCENT (95),
#      SCORECARD_PATH (chaos_scorecard.json).
# Exit: 0 when every experiment keeps availability at or above the threshold.
set -euo pipefail

: "${API_URL:?API_URL is required}"
DURATION="${CHAOS_DURATION_SECS:-60}"
INTERVAL="${PROBE_INTERVAL_SECS:-2}"
MIN_AVAILABILITY="${MIN_AVAILABILITY_PERCENT:-95}"
SCORECARD="${SCORECARD_PATH:-chaos_scorecard.json}"
SUITE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/chaos_suite.yaml"

names="$(awk '/^  name: /{print $2}' "${SUITE}")"
[[ -n "${names}" ]] || { echo "no experiments found in ${SUITE}" >&2; exit 1; }
trap 'kubectl delete -f "${SUITE}" --ignore-not-found >/dev/null 2>&1 || true' EXIT

results='[]'
for name in ${names}; do
  doc="$(kubectl create --dry-run=client -f "${SUITE}" -o json | jq -c --arg n "${name}" '.items[] | select(.metadata.name == $n)')"
  echo "${doc}" | kubectl apply -f - >/dev/null
  total=0; ok=0
  end=$(( $(date +%s) + DURATION ))
  while [[ "$(date +%s)" -lt "${end}" ]]; do
    total=$(( total + 1 ))
    # Availability = the API still answers /health with 2xx within 5s.
    if curl -fsS --max-time 5 -o /dev/null "${API_URL}/health"; then ok=$(( ok + 1 )); fi
    sleep "${INTERVAL}"
  done
  echo "${doc}" | kubectl delete -f - --ignore-not-found >/dev/null
  results="$(jq -c --arg n "${name}" --argjson t "${total}" --argjson o "${ok}" --argjson min "${MIN_AVAILABILITY}" \
    '. + [{experiment: $n, probes: $t, successful: $o,
           availability_percent: (if $t == 0 then 0 else ($o * 10000 / $t | round / 100) end)}
          | .passed = (.availability_percent >= $min)]' <<<"${results}")"
done

jq -n --argjson r "${results}" --argjson min "${MIN_AVAILABILITY}" \
  '{generated_at: (now | todate), min_availability_percent: $min,
    score_percent: ($r | (map(select(.passed)) | length) * 100 / length | round),
    passed: ($r | all(.passed)), experiments: $r}' | tee "${SCORECARD}"
jq -e '.passed' "${SCORECARD}" >/dev/null
