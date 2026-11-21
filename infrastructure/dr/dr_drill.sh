#!/usr/bin/env bash
# Automated weekly disaster-recovery drill — issue #393.
#
# Brings the warm ECS Fargate standby up, restores the latest pgBackRest WAL
# backup from S3/GCS, waits for the read-only API to become healthy, runs the API
# test suite against the restored replica, measures RTO/RPO and reports the
# outcome to PagerDuty (plus optional Slack). `restore_verification.sh --drill`
# is the stable entry point invoked by .github/workflows/dr_drill.yml.
#
# Required environment (all have production-oriented defaults):
#   DR_CLUSTER / DR_SERVICE / DR_REGION    standby ECS identifiers
#   DR_REPLICA_HOST / DR_REPLICA_PORT      restored Postgres endpoint
#   STANDBY_HEALTH_URL                     standby API /health URL
#   PGBACKREST_STANZA                      pgBackRest stanza (default prod)
#   FAILOVER_WORKER_URL                    DR worker /__failover/check endpoint
#   PAGERDUTY_ROUTING_KEY                  Events API v2 key (optional)
#   SLACK_WEBHOOK_URL                      optional Slack report
#   DR_RTO_TARGET_SECONDS                  900 (15 min)
#   DR_RPO_TARGET_SECONDS                  300 (5 min)
#
# Exit codes: 0 pass, 1 hard failure, 2 objective missed (RTO/RPO breach).
set -euo pipefail

readonly RUN_ID="dr-drill-$(date +%Y%m%dT%H%M%SZ)-$$"
readonly START_EPOCH="$(date +%s)"
readonly REPORT_PATH="${DR_REPORT_PATH:-/tmp/tessera-dr-drill.json}"
readonly ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

DR_CLUSTER="${DR_CLUSTER:-tessera-prod-dr-cluster}"
DR_SERVICE="${DR_SERVICE:-tessera-prod-dr-api-service}"
DR_REGION="${DR_REGION:-us-east-1}"
DR_DESIRED_COUNT="${DR_DESIRED_COUNT:-1}"
DR_REPLICA_HOST="${DR_REPLICA_HOST:-localhost}"
DR_REPLICA_PORT="${DR_REPLICA_PORT:-5432}"
STANDBY_HEALTH_URL="${STANDBY_HEALTH_URL:-http://localhost:8080/health}"
PGBACKREST_STANZA="${PGBACKREST_STANZA:-prod}"
PGBACKREST_REPO="${PGBACKREST_REPO:-1}"
FAILOVER_WORKER_URL="${FAILOVER_WORKER_URL:-}"
PAGERDUTY_ROUTING_KEY="${PAGERDUTY_ROUTING_KEY:-}"
SLACK_WEBHOOK_URL="${SLACK_WEBHOOK_URL:-}"
DR_RTO_TARGET_SECONDS="${DR_RTO_TARGET_SECONDS:-900}"
DR_RPO_TARGET_SECONDS="${DR_RPO_TARGET_SECONDS:-300}"
DRY_RUN="${DRY_RUN:-false}"

STAGES=()
WARNINGS=0
ERRORS=0

log() { printf '[dr-drill] %s %s\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ')" "$*" >&2; }
stage() {
  local name="$1" start epoch duration status="$2"
  start="$(date +%s)"
  log "START ${name}"
  shift 2
  "$@" || return $?
  epoch="$(date +%s)"
  duration=$(( epoch - start ))
  STAGES+=("{\"name\":\"${name}\",\"status\":\"${status}\",\"duration_seconds\":${duration}}")
  log "DONE  ${name} in ${duration}s [${status}]"
}

fail() { log "ERROR $*"; ERRORS=$((ERRORS + 1)); }

# ---------------------------------------------------------------------------
# 1. Bring up the warm standby (RTO starts here).
# ---------------------------------------------------------------------------
scale_standby() {
  if [[ "${DRY_RUN}" == "true" ]]; then
    log "[DRY-RUN] would scale ${DR_SERVICE} to ${DR_DESIRED_COUNT}"
    return 0
  fi
  aws ecs update-service \
    --cluster "${DR_CLUSTER}" --service "${DR_SERVICE}" \
    --desired-count "${DR_DESIRED_COUNT}" --region "${DR_REGION}" >/dev/null
  aws ecs wait services-stable \
    --cluster "${DR_CLUSTER}" --services "${DR_SERVICE}" --region "${DR_REGION}"
}

# ---------------------------------------------------------------------------
# 2. Restore the newest archived WAL from pgBackRest (S3 or GCS).
# ---------------------------------------------------------------------------
restore_wal() {
  if [[ "${DRY_RUN}" == "true" ]]; then
    log "[DRY-RUN] would restore stanza ${PGBACKREST_STANZA} from repo ${PGBACKREST_REPO}"
    return 0
  fi
  pgbackrest --stanza="${PGBACKREST_STANZA}" --repo="${PGBACKREST_REPO}" \
    --pg1-path=/var/lib/postgresql/16/main \
    restore --type=default --delta --no-owner --no-privileges \
    --target-action=promote
  psql -h "${DR_REPLICA_HOST}" -p "${DR_REPLICA_PORT}" -U postgres -d postgres \
    -Atqc 'SELECT pg_is_in_recovery();' >/dev/null
}

# ---------------------------------------------------------------------------
# 3. Wait for the read-only API to answer /health.
# ---------------------------------------------------------------------------
wait_for_health() {
  local deadline=$(( START_EPOCH + DR_RTO_TARGET_SECONDS )) attempt=0
  if [[ "${DRY_RUN}" == "true" ]]; then return 0; fi
  while (( $(date +%s) < deadline )); do
    attempt=$((attempt + 1))
    if curl -fsS --max-time 5 "${STANDBY_HEALTH_URL}" >/dev/null 2>&1; then
      RTO_SECONDS=$(( $(date +%s) - START_EPOCH ))
      log "standby healthy after ${RTO_SECONDS}s (${attempt} probes)"
      return 0
    fi
    sleep 5
  done
  fail "standby did not become healthy within ${DR_RTO_TARGET_SECONDS}s"
  return 1
}

# ---------------------------------------------------------------------------
# 4. Exercise the restored replica with the real API test suite.
# ---------------------------------------------------------------------------
run_api_tests() {
  if [[ "${DRY_RUN}" == "true" ]]; then
    log "[DRY-RUN] would run the API test suite"
    return 0
  fi
  RWA_DATABASE_URL="postgresql://postgres@${DR_REPLICA_HOST}:${DR_REPLICA_PORT}/tessera_dr_test" \
    cargo test --manifest-path "${ROOT_DIR}/api/Cargo.toml" --tests
}

# ---------------------------------------------------------------------------
# 5. Measure RPO from the newest archived WAL segment timestamp.
# ---------------------------------------------------------------------------
measure_rpo() {
  local info last_epoch
  if [[ "${DRY_RUN}" == "true" ]]; then
    RPO_SECONDS=0
    return 0
  fi
  info="$(pgbackrest --stanza="${PGBACKREST_STANZA}" --output=json info)"
  last_epoch="$(printf '%s' "${info}" | jq -r '[.[] | .backup[]?.timestamp.stop // empty] | max // 0')"
  if [[ "${last_epoch}" -eq 0 ]]; then
    fail "no archived WAL found; RPO is unbounded"
    return 1
  fi
  RPO_SECONDS=$(( START_EPOCH - last_epoch ))
  if (( RPO_SECONDS < 0 )); then RPO_SECONDS=0; fi
  log "measured RPO ${RPO_SECONDS}s (target ${DR_RPO_TARGET_SECONDS}s)"
}

# ---------------------------------------------------------------------------
# 6. Return production weights to the primary after a successful drill.
# ---------------------------------------------------------------------------
reset_failover() {
  [[ -z "${FAILOVER_WORKER_URL}" ]] && return 0
  if [[ "${DRY_RUN}" == "true" ]]; then
    log "[DRY-RUN] would call ${FAILOVER_WORKER_URL}"
    return 0
  fi
  curl -fsS --max-time 10 "${FAILOVER_WORKER_URL}" >/dev/null || \
    log "WARN could not re-check primary routing; verify the LB state manually"
}

write_report() {
  local overall="$1" stages joined=""
  for stage_json in "${STAGES[@]}"; do
    [[ -n "${joined}" ]] && joined+=","
    joined+="${stage_json}"
  done
  jq -n \
    --arg run_id "${RUN_ID}" \
    --arg overall "${overall}" \
    --argjson rto "${RTO_SECONDS:-0}" \
    --argjson rpo "${RPO_SECONDS:-0}" \
    --argjson rto_target "${DR_RTO_TARGET_SECONDS}" \
    --argjson rpo_target "${DR_RPO_TARGET_SECONDS}" \
    --argjson stages "[${joined}]" \
    --argjson errors "${ERRORS}" \
    '{
      run_id: $run_id,
      timestamp_utc: (now | strftime("%Y-%m-%dT%H:%M:%SZ")),
      result: $overall,
      objectives: {
        rto_seconds: $rto, rto_target_seconds: $rto_target,
        rpo_seconds: $rpo, rpo_target_seconds: $rpo_target,
        rto_met: ($rto <= $rto_target), rpo_met: ($rpo <= $rpo_target)
      },
      stages: $stages,
      errors: $errors
    }' > "${REPORT_PATH}"
  log "report written to ${REPORT_PATH}"
}

notify() {
  local overall="$1" severity="info"
  [[ "${overall}" != "success" ]] && severity="critical"
  if [[ -n "${PAGERDUTY_ROUTING_KEY}" && "${overall}" != "success" ]]; then
    curl -fsS -X POST https://events.pagerduty.com/v2/enqueue \
      -H 'Content-Type: application/json' \
      -d "$(jq -n --arg key "${PAGERDUTY_ROUTING_KEY}" --arg run "${RUN_ID}" \
          --argjson rto "${RTO_SECONDS:-0}" --argjson rpo "${RPO_SECONDS:-0}" \
          '{routing_key:$key,event_action:"trigger",dedup_key:("tessera-dr-drill-"+$run),
            payload:{summary:("Tessera DR drill failed: "+$run),severity:"critical",
            source:"dr_drill.sh",custom_details:{run_id:$run,rto_seconds:$rto,rpo_seconds:$rpo}}}')" \
      >/dev/null || log "WARN PagerDuty notification failed"
  fi
  if [[ -n "${SLACK_WEBHOOK_URL}" ]]; then
    curl -fsS -X POST -H 'Content-type: application/json' \
      -d "$(jq -nc --arg run "${RUN_ID}" --arg status "${overall}" \
          '{text:("Tessera DR drill "+$status+" (run "+$run+")")}')" \
      "${SLACK_WEBHOOK_URL}" >/dev/null || true
  fi
}

main() {
  log "=== Tessera DR drill ${RUN_ID} ==="
  stage "scale-standby" "ok" scale_standby || { fail "standby scale-up failed"; }
  if [[ "${ERRORS}" -eq 0 ]]; then
    stage "restore-latest-wal" "ok" restore_wal || fail "WAL restore failed"
  fi
  if [[ "${ERRORS}" -eq 0 ]]; then
    stage "wait-for-health" "ok" wait_for_health || true
  fi
  if [[ "${ERRORS}" -eq 0 ]]; then
    stage "api-test-suite" "ok" run_api_tests || fail "API tests failed on restored data"
  fi
  stage "measure-rpo" "ok" measure_rpo || true
  stage "reset-routing" "ok" reset_failover || true

  local overall="success" exit_code=0
  if (( ERRORS > 0 )); then
    overall="failure"; exit_code=1
  elif (( RTO_SECONDS > DR_RTO_TARGET_SECONDS || RPO_SECONDS > DR_RPO_TARGET_SECONDS )); then
    overall="objective_missed"; exit_code=2
    WARNINGS=$((WARNINGS + 1))
  fi
  write_report "${overall}"
  notify "${overall}"
  log "=== DR drill ${overall}: RTO=${RTO_SECONDS:-0}s/${DR_RTO_TARGET_SECONDS}s RPO=${RPO_SECONDS:-0}s/${DR_RPO_TARGET_SECONDS}s ==="
  exit "${exit_code}"
}

main "$@"
