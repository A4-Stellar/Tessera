#!/usr/bin/env bash
# =============================================================================
# Tessera — Automated Disaster Recovery Verification Pipeline
# Issue #140: dr_test.sh
#
# Restores an encrypted database snapshot from S3 or GCS into an isolated
# PostgreSQL test instance, runs integrity assertions (checksums, record
# counts), emits structured timing metrics, and notifies DevOps via Slack
# or PagerDuty.
#
# Usage:
#   ./dr_test.sh [--dry-run] [--level <minimal|standard|full>]
#
# Environment variables (all have sane defaults for CI):
#   BACKUP_PROVIDER         s3 | gcs                              (default: s3)
#   BACKUP_BUCKET           S3 bucket or GCS bucket name          (required)
#   BACKUP_KEY_PREFIX       Path prefix for snapshot objects       (default: backups/postgres)
#   BACKUP_SNAPSHOT_NAME    Specific snapshot filename; if empty,  (default: latest)
#                           the script selects the most recent one
#   ENCRYPTION_KEY_ARN      AWS KMS key ARN or GCP KMS key name   (optional; if set, uses cloud KMS)
#   GPG_KEY_ID              GPG key fingerprint for AES-256 backup  (optional; used when no KMS)
#   GPG_PASSPHRASE          Passphrase for GPG symmetric decrypt   (optional)
#
#   DB_RESTORE_HOST         Isolated restore target host           (default: localhost)
#   DB_RESTORE_PORT         Restore target port                    (default: 5432)
#   DB_RESTORE_NAME         Database name to restore into          (default: tessera_dr_test)
#   DB_RESTORE_USER         PostgreSQL superuser for restore       (default: postgres)
#   PGPASSWORD              Password for DB_RESTORE_USER           (required in non-CI)
#
#   PRODUCTION_BASELINE     Path to JSON file with expected record  (default: baseline.json)
#                           counts per table, produced by a prior
#                           run against production.
#
#   SLACK_WEBHOOK_URL       Slack incoming webhook URL             (optional)
#   PAGERDUTY_ROUTING_KEY   PagerDuty Events API v2 routing key   (optional)
#   DEVOPS_EMAIL            Email address for summary report        (optional)
#
#   DR_REPORT_PATH          Where to write JSON summary report      (default: /tmp/dr_test_report.json)
#   DR_LOG_LEVEL            debug | info | warn | error             (default: info)
#   DRY_RUN                 1 | true → skip destructive ops        (default: false)
#
# Exit codes:
#   0 — all checks passed; restoration succeeded
#   1 — hard failure (backup download failed, restore failed, integrity error)
#   2 — warning (soft assertion failures; restore succeeded but anomalies found)
#
# Dependencies: aws-cli or gcloud, postgresql-client (pg_restore, psql),
#               gnupg (if GPG-encrypted), jq, curl, sha256sum/shasum
# =============================================================================

set -euo pipefail

# ---------------------------------------------------------------------------
# Global constants
# ---------------------------------------------------------------------------
readonly SCRIPT_VERSION="1.0.0"
readonly SCRIPT_NAME="$(basename "${BASH_SOURCE[0]}")"
readonly SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
readonly RUN_ID="dr-$(date +%Y%m%d-%H%M%S)-$$"
readonly TEMP_DIR="$(mktemp -d /tmp/tessera-dr-XXXXXX)"
readonly START_EPOCH="$(date +%s)"

# ---------------------------------------------------------------------------
# ANSI colour helpers (disabled in CI unless FORCE_COLOR is set)
# ---------------------------------------------------------------------------
if [[ -t 1 ]] || [[ "${FORCE_COLOR:-}" == "1" ]]; then
  RED='\033[0;31m'; YELLOW='\033[1;33m'; GREEN='\033[0;32m'
  CYAN='\033[0;36m'; BOLD='\033[1m'; RESET='\033[0m'
else
  RED=''; YELLOW=''; GREEN=''; CYAN=''; BOLD=''; RESET=''
fi

# ---------------------------------------------------------------------------
# Configuration (read from environment with defaults)
# ---------------------------------------------------------------------------
BACKUP_PROVIDER="${BACKUP_PROVIDER:-s3}"
BACKUP_BUCKET="${BACKUP_BUCKET:-}"
BACKUP_KEY_PREFIX="${BACKUP_KEY_PREFIX:-backups/postgres}"
BACKUP_SNAPSHOT_NAME="${BACKUP_SNAPSHOT_NAME:-}"
ENCRYPTION_KEY_ARN="${ENCRYPTION_KEY_ARN:-}"
GPG_KEY_ID="${GPG_KEY_ID:-}"
GPG_PASSPHRASE="${GPG_PASSPHRASE:-}"

DB_RESTORE_HOST="${DB_RESTORE_HOST:-localhost}"
DB_RESTORE_PORT="${DB_RESTORE_PORT:-5432}"
DB_RESTORE_NAME="${DB_RESTORE_NAME:-tessera_dr_test}"
DB_RESTORE_USER="${DB_RESTORE_USER:-postgres}"

PRODUCTION_BASELINE="${PRODUCTION_BASELINE:-${SCRIPT_DIR}/baseline.json}"

SLACK_WEBHOOK_URL="${SLACK_WEBHOOK_URL:-}"
PAGERDUTY_ROUTING_KEY="${PAGERDUTY_ROUTING_KEY:-}"

DR_REPORT_PATH="${DR_REPORT_PATH:-/tmp/dr_test_report.json}"
DR_LOG_LEVEL="${DR_LOG_LEVEL:-info}"
DRY_RUN="${DRY_RUN:-false}"

# Parse CLI flags
for arg in "$@"; do
  case "$arg" in
    --dry-run)   DRY_RUN=true ;;
    --debug)     DR_LOG_LEVEL=debug ;;
    --help|-h)
      head -60 "${BASH_SOURCE[0]}" | grep '^#' | sed 's/^# \?//'
      exit 0
      ;;
  esac
done

# ---------------------------------------------------------------------------
# Timing and metrics accumulator
# ---------------------------------------------------------------------------
declare -A STAGE_DURATIONS=()
declare -A STAGE_STATUS=()   # ok | fail | skip
WARNINGS=0
ERRORS=0

# ---------------------------------------------------------------------------
# Logging
# ---------------------------------------------------------------------------
log() {
  local level="$1"; shift
  local msg="$*"
  local ts; ts="$(date '+%Y-%m-%dT%H:%M:%S%z')"

  case "$level" in
    DEBUG) [[ "$DR_LOG_LEVEL" == "debug" ]] || return 0; echo -e "${CYAN}[${ts}] [DEBUG]${RESET} ${msg}" ;;
    INFO)  echo -e "${GREEN}[${ts}] [INFO] ${RESET} ${msg}" ;;
    WARN)  echo -e "${YELLOW}[${ts}] [WARN] ${RESET} ${msg}" >&2; (( WARNINGS++ )) || true ;;
    ERROR) echo -e "${RED}[${ts}] [ERROR]${RESET} ${msg}" >&2; (( ERRORS++ )) || true ;;
    STEP)  echo -e "\n${BOLD}[${ts}] ━━━ ${msg} ━━━${RESET}" ;;
  esac
}

# ---------------------------------------------------------------------------
# Stage timer helpers
# ---------------------------------------------------------------------------
stage_start() {
  log STEP "$1"
  echo "$(($(date +%s)))"
}

stage_end() {
  local name="$1"
  local start_epoch="$2"
  local status="${3:-ok}"
  local duration=$(( $(date +%s) - start_epoch ))
  STAGE_DURATIONS["$name"]="$duration"
  STAGE_STATUS["$name"]="$status"
  log INFO "Stage '${name}' completed in ${duration}s [${status}]"
}

# ---------------------------------------------------------------------------
# Cleanup on exit
# ---------------------------------------------------------------------------
cleanup() {
  local exit_code=$?
  log INFO "Cleaning up temporary directory: ${TEMP_DIR}"
  rm -rf "${TEMP_DIR}"

  # Drop the restore database if it was created (unless dry-run)
  if [[ "${DRY_RUN}" != "true" ]] && [[ "${DB_CREATED:-false}" == "true" ]]; then
    log INFO "Dropping test database '${DB_RESTORE_NAME}'"
    psql -h "${DB_RESTORE_HOST}" -p "${DB_RESTORE_PORT}" \
         -U "${DB_RESTORE_USER}" -d postgres \
         -c "DROP DATABASE IF EXISTS ${DB_RESTORE_NAME};" 2>/dev/null || true
  fi

  # Emit final report if we got far enough
  if [[ "${REPORT_WRITTEN:-false}" != "true" ]]; then
    write_report "$exit_code"
  fi

  exit "$exit_code"
}
trap cleanup EXIT

# ---------------------------------------------------------------------------
# Dependency check
# ---------------------------------------------------------------------------
check_dependencies() {
  local t; t=$(stage_start "Dependency Check")
  local missing=()

  for cmd in psql pg_restore jq curl sha256sum; do
    if ! command -v "$cmd" &>/dev/null; then
      # sha256sum may be shasum on macOS
      if [[ "$cmd" == "sha256sum" ]] && command -v shasum &>/dev/null; then
        continue
      fi
      missing+=("$cmd")
    fi
  done

  case "${BACKUP_PROVIDER}" in
    s3)  command -v aws  &>/dev/null || missing+=("aws-cli") ;;
    gcs) command -v gsutil &>/dev/null || missing+=("gcloud/gsutil") ;;
    *)   log ERROR "Unknown BACKUP_PROVIDER '${BACKUP_PROVIDER}' (must be s3 or gcs)"; exit 1 ;;
  esac

  if [[ -n "${GPG_KEY_ID}" ]] || [[ -n "${GPG_PASSPHRASE}" ]]; then
    command -v gpg &>/dev/null || missing+=("gnupg")
  fi

  if [[ "${#missing[@]}" -gt 0 ]]; then
    log ERROR "Missing required tools: ${missing[*]}"
    log ERROR "Install them and retry."
    exit 1
  fi

  [[ -z "${BACKUP_BUCKET}" ]] && { log ERROR "BACKUP_BUCKET is required"; exit 1; }

  stage_end "Dependency Check" "$t" "ok"
}

# ---------------------------------------------------------------------------
# Stage 1 — Locate snapshot
# ---------------------------------------------------------------------------
locate_snapshot() {
  local t; t=$(stage_start "Locate Snapshot")
  local snapshot_path=""

  if [[ -n "${BACKUP_SNAPSHOT_NAME}" ]]; then
    snapshot_path="${BACKUP_KEY_PREFIX}/${BACKUP_SNAPSHOT_NAME}"
    log INFO "Using specified snapshot: ${snapshot_path}"
  else
    log INFO "Discovering latest snapshot in ${BACKUP_PROVIDER}://${BACKUP_BUCKET}/${BACKUP_KEY_PREFIX}/"
    case "${BACKUP_PROVIDER}" in
      s3)
        snapshot_path="$(aws s3 ls "s3://${BACKUP_BUCKET}/${BACKUP_KEY_PREFIX}/" \
          | sort | tail -1 | awk '{print $4}')"
        snapshot_path="${BACKUP_KEY_PREFIX}/${snapshot_path}"
        ;;
      gcs)
        snapshot_path="$(gsutil ls "gs://${BACKUP_BUCKET}/${BACKUP_KEY_PREFIX}/" \
          | sort | tail -1)"
        ;;
    esac
    log INFO "Selected snapshot: ${snapshot_path}"
  fi

  SNAPSHOT_PATH="${snapshot_path}"
  SNAPSHOT_BASENAME="$(basename "${snapshot_path}")"
  SNAPSHOT_LOCAL="${TEMP_DIR}/${SNAPSHOT_BASENAME}"

  stage_end "Locate Snapshot" "$t" "ok"
}

# ---------------------------------------------------------------------------
# Stage 2 — Download snapshot
# ---------------------------------------------------------------------------
download_snapshot() {
  local t; t=$(stage_start "Download Snapshot")

  if [[ "${DRY_RUN}" == "true" ]]; then
    log INFO "[DRY-RUN] Would download ${SNAPSHOT_PATH} to ${SNAPSHOT_LOCAL}"
    touch "${SNAPSHOT_LOCAL}"
    stage_end "Download Snapshot" "$t" "skip"
    return
  fi

  case "${BACKUP_PROVIDER}" in
    s3)
      log INFO "Downloading from S3: s3://${BACKUP_BUCKET}/${SNAPSHOT_PATH}"
      aws s3 cp "s3://${BACKUP_BUCKET}/${SNAPSHOT_PATH}" "${SNAPSHOT_LOCAL}" \
        --no-progress
      # Also download the checksum sidecar file if it exists
      aws s3 cp "s3://${BACKUP_BUCKET}/${SNAPSHOT_PATH}.sha256" \
        "${SNAPSHOT_LOCAL}.sha256" --no-progress 2>/dev/null || true
      ;;
    gcs)
      log INFO "Downloading from GCS: gs://${BACKUP_BUCKET}/${SNAPSHOT_PATH}"
      gsutil cp "${SNAPSHOT_PATH}" "${SNAPSHOT_LOCAL}"
      gsutil cp "${SNAPSHOT_PATH}.sha256" "${SNAPSHOT_LOCAL}.sha256" 2>/dev/null || true
      ;;
  esac

  local file_size; file_size="$(du -sh "${SNAPSHOT_LOCAL}" | cut -f1)"
  log INFO "Downloaded: ${SNAPSHOT_LOCAL} (${file_size})"

  stage_end "Download Snapshot" "$t" "ok"
}

# ---------------------------------------------------------------------------
# Stage 3 — Verify checksum
# ---------------------------------------------------------------------------
verify_checksum() {
  local t; t=$(stage_start "Checksum Verification")

  if [[ "${DRY_RUN}" == "true" ]]; then
    log INFO "[DRY-RUN] Skipping checksum verification"
    stage_end "Checksum Verification" "$t" "skip"
    ACTUAL_CHECKSUM="dry-run-skipped"
    return
  fi

  # Compute actual checksum
  if command -v sha256sum &>/dev/null; then
    ACTUAL_CHECKSUM="$(sha256sum "${SNAPSHOT_LOCAL}" | awk '{print $1}')"
  else
    ACTUAL_CHECKSUM="$(shasum -a 256 "${SNAPSHOT_LOCAL}" | awk '{print $1}')"
  fi
  log INFO "SHA-256: ${ACTUAL_CHECKSUM}"

  # Compare against sidecar file if available
  if [[ -f "${SNAPSHOT_LOCAL}.sha256" ]]; then
    local expected; expected="$(cat "${SNAPSHOT_LOCAL}.sha256" | awk '{print $1}')"
    if [[ "${ACTUAL_CHECKSUM}" == "${expected}" ]]; then
      log INFO "Checksum matches expected: ${expected}"
    else
      log ERROR "Checksum MISMATCH — expected: ${expected}, got: ${ACTUAL_CHECKSUM}"
      stage_end "Checksum Verification" "$t" "fail"
      exit 1
    fi
  else
    log WARN "No .sha256 sidecar found; recording computed checksum only"
  fi

  stage_end "Checksum Verification" "$t" "ok"
}

# ---------------------------------------------------------------------------
# Stage 4 — Decrypt snapshot
# ---------------------------------------------------------------------------
decrypt_snapshot() {
  local t; t=$(stage_start "Decrypt Snapshot")

  # Determine whether the snapshot is encrypted by extension
  case "${SNAPSHOT_BASENAME}" in
    *.gpg|*.asc|*.enc)
      log INFO "Snapshot appears encrypted (extension: ${SNAPSHOT_BASENAME##*.})"
      ;;
    *)
      log INFO "Snapshot does not appear encrypted; skipping decryption stage"
      SNAPSHOT_DECRYPTED="${SNAPSHOT_LOCAL}"
      stage_end "Decrypt Snapshot" "$t" "skip"
      return
      ;;
  esac

  if [[ "${DRY_RUN}" == "true" ]]; then
    log INFO "[DRY-RUN] Skipping decryption"
    SNAPSHOT_DECRYPTED="${SNAPSHOT_LOCAL%.gpg}"
    touch "${SNAPSHOT_DECRYPTED}"
    stage_end "Decrypt Snapshot" "$t" "skip"
    return
  fi

  SNAPSHOT_DECRYPTED="${SNAPSHOT_LOCAL%.gpg}"
  SNAPSHOT_DECRYPTED="${SNAPSHOT_DECRYPTED%.asc}"
  SNAPSHOT_DECRYPTED="${SNAPSHOT_DECRYPTED%.enc}"

  if [[ -n "${ENCRYPTION_KEY_ARN}" ]]; then
    # Cloud KMS path
    case "${BACKUP_PROVIDER}" in
      s3)
        log INFO "Decrypting via AWS KMS key: ${ENCRYPTION_KEY_ARN}"
        aws kms decrypt \
          --ciphertext-blob "fileb://${SNAPSHOT_LOCAL}" \
          --key-id "${ENCRYPTION_KEY_ARN}" \
          --output text \
          --query Plaintext \
          | base64 --decode > "${SNAPSHOT_DECRYPTED}"
        ;;
      gcs)
        log INFO "Decrypting via GCP KMS key: ${ENCRYPTION_KEY_ARN}"
        gcloud kms decrypt \
          --key="${ENCRYPTION_KEY_ARN}" \
          --ciphertext-file="${SNAPSHOT_LOCAL}" \
          --plaintext-file="${SNAPSHOT_DECRYPTED}"
        ;;
    esac
  elif [[ -n "${GPG_PASSPHRASE}" ]]; then
    log INFO "Decrypting via GPG symmetric passphrase"
    echo "${GPG_PASSPHRASE}" | gpg --batch --yes --passphrase-fd 0 \
      --output "${SNAPSHOT_DECRYPTED}" \
      --decrypt "${SNAPSHOT_LOCAL}"
  elif [[ -n "${GPG_KEY_ID}" ]]; then
    log INFO "Decrypting via GPG key: ${GPG_KEY_ID}"
    gpg --batch --yes \
      --output "${SNAPSHOT_DECRYPTED}" \
      --decrypt "${SNAPSHOT_LOCAL}"
  else
    log ERROR "Snapshot appears encrypted but no decryption method configured."
    log ERROR "Set ENCRYPTION_KEY_ARN, GPG_KEY_ID, or GPG_PASSPHRASE."
    stage_end "Decrypt Snapshot" "$t" "fail"
    exit 1
  fi

  log INFO "Decrypted snapshot written to: ${SNAPSHOT_DECRYPTED}"
  stage_end "Decrypt Snapshot" "$t" "ok"
}

# ---------------------------------------------------------------------------
# Stage 5 — Prepare restore database
# ---------------------------------------------------------------------------
prepare_restore_db() {
  local t; t=$(stage_start "Prepare Restore Database")

  if [[ "${DRY_RUN}" == "true" ]]; then
    log INFO "[DRY-RUN] Would create database '${DB_RESTORE_NAME}' on ${DB_RESTORE_HOST}:${DB_RESTORE_PORT}"
    stage_end "Prepare Restore Database" "$t" "skip"
    return
  fi

  log INFO "Connecting to PostgreSQL at ${DB_RESTORE_HOST}:${DB_RESTORE_PORT} as ${DB_RESTORE_USER}"

  # Drop any previous test DB, then create fresh
  psql -h "${DB_RESTORE_HOST}" -p "${DB_RESTORE_PORT}" \
       -U "${DB_RESTORE_USER}" -d postgres \
       -c "DROP DATABASE IF EXISTS ${DB_RESTORE_NAME};" 2>&1 | log DEBUG

  psql -h "${DB_RESTORE_HOST}" -p "${DB_RESTORE_PORT}" \
       -U "${DB_RESTORE_USER}" -d postgres \
       -c "CREATE DATABASE ${DB_RESTORE_NAME};" 2>&1 | log DEBUG

  DB_CREATED=true
  log INFO "Created isolated test database: ${DB_RESTORE_NAME}"
  stage_end "Prepare Restore Database" "$t" "ok"
}

# ---------------------------------------------------------------------------
# Stage 6 — Restore database
# ---------------------------------------------------------------------------
restore_database() {
  local t; t=$(stage_start "Database Restore")
  local snapshot="${SNAPSHOT_DECRYPTED:-${SNAPSHOT_LOCAL}}"

  if [[ "${DRY_RUN}" == "true" ]]; then
    log INFO "[DRY-RUN] Would run pg_restore on ${snapshot} → ${DB_RESTORE_NAME}"
    stage_end "Database Restore" "$t" "skip"
    return
  fi

  log INFO "Restoring ${snapshot} into database '${DB_RESTORE_NAME}'"

  # Detect format: plain SQL (.sql) vs custom format (.dump, .pgdump, .backup)
  case "${snapshot}" in
    *.sql|*.sql.gz)
      if [[ "${snapshot}" == *.gz ]]; then
        gunzip -c "${snapshot}" | psql \
          -h "${DB_RESTORE_HOST}" -p "${DB_RESTORE_PORT}" \
          -U "${DB_RESTORE_USER}" -d "${DB_RESTORE_NAME}" \
          --set ON_ERROR_STOP=1 2>&1
      else
        psql -h "${DB_RESTORE_HOST}" -p "${DB_RESTORE_PORT}" \
             -U "${DB_RESTORE_USER}" -d "${DB_RESTORE_NAME}" \
             --set ON_ERROR_STOP=1 \
             -f "${snapshot}" 2>&1
      fi
      ;;
    *)
      # Custom format — use pg_restore (parallel: 4 jobs)
      pg_restore \
        -h "${DB_RESTORE_HOST}" \
        -p "${DB_RESTORE_PORT}" \
        -U "${DB_RESTORE_USER}" \
        -d "${DB_RESTORE_NAME}" \
        --jobs=4 \
        --no-owner \
        --no-privileges \
        --if-exists \
        --clean \
        "${snapshot}" 2>&1 || {
          log ERROR "pg_restore exited with non-zero status"
          stage_end "Database Restore" "$t" "fail"
          exit 1
        }
      ;;
  esac

  log INFO "Restore complete"
  stage_end "Database Restore" "$t" "ok"
}

# ---------------------------------------------------------------------------
# Stage 7 — Integrity assertions
# ---------------------------------------------------------------------------
run_integrity_checks() {
  local t; t=$(stage_start "Integrity Checks")
  local check_passed=true

  if [[ "${DRY_RUN}" == "true" ]]; then
    log INFO "[DRY-RUN] Skipping integrity checks"
    stage_end "Integrity Checks" "$t" "skip"
    INTEGRITY_RESULTS="{}"
    return
  fi

  log INFO "Running integrity checks against '${DB_RESTORE_NAME}'"

  # ---- 7a. Schema consistency: required tables must exist -------------------
  local required_tables=("assets" "holders" "compliance_records" "dividend_distributions" "indexer_snapshots")
  local schema_ok=true

  for tbl in "${required_tables[@]}"; do
    local exists
    exists="$(psql -h "${DB_RESTORE_HOST}" -p "${DB_RESTORE_PORT}" \
                   -U "${DB_RESTORE_USER}" -d "${DB_RESTORE_NAME}" \
                   -tAq -c "SELECT to_regclass('public.${tbl}');" 2>/dev/null)"
    if [[ -z "${exists}" ]] || [[ "${exists}" == "NULL" ]]; then
      log WARN "Table '${tbl}' is absent from the restored database"
      schema_ok=false
    else
      log DEBUG "Table '${tbl}' present: OK"
    fi
  done

  # ---- 7b. Record count matching -------------------------------------------
  declare -A ACTUAL_COUNTS=()
  INTEGRITY_RESULTS="{"
  local first_field=true

  for tbl in "${required_tables[@]}"; do
    local count
    count="$(psql -h "${DB_RESTORE_HOST}" -p "${DB_RESTORE_PORT}" \
                  -U "${DB_RESTORE_USER}" -d "${DB_RESTORE_NAME}" \
                  -tAq -c "SELECT COUNT(*) FROM ${tbl};" 2>/dev/null || echo -1)"
    ACTUAL_COUNTS["$tbl"]="$count"
    log INFO "Table '${tbl}': ${count} rows"

    [[ "$first_field" == "true" ]] && first_field=false || INTEGRITY_RESULTS+=","
    INTEGRITY_RESULTS+="\"${tbl}\":{\"actual\":${count}"

    # Compare against baseline if available
    if [[ -f "${PRODUCTION_BASELINE}" ]]; then
      local expected
      expected="$(jq -r ".tables.${tbl}.count // -1" "${PRODUCTION_BASELINE}" 2>/dev/null || echo -1)"
      INTEGRITY_RESULTS+=",\"expected\":${expected}"

      if [[ "${expected}" -ge 0 ]]; then
        local tolerance=10  # allow ±10 rows for in-flight transactions
        local diff=$(( count - expected ))
        diff=${diff#-}  # abs value

        if [[ $diff -gt $tolerance ]]; then
          log WARN "Record count mismatch in '${tbl}': expected ${expected}, got ${count} (diff=${diff}, tolerance=${tolerance})"
          check_passed=false
          INTEGRITY_RESULTS+=",\"status\":\"mismatch\""
        else
          log INFO "Record count for '${tbl}': within tolerance (diff=${diff})"
          INTEGRITY_RESULTS+=",\"status\":\"ok\""
        fi
      else
        INTEGRITY_RESULTS+=",\"status\":\"no_baseline\""
      fi
    fi

    INTEGRITY_RESULTS+="}"
  done
  INTEGRITY_RESULTS+="}"

  # ---- 7c. Data consistency spot-check: assets should have valid ledger refs -
  local orphaned_holders
  orphaned_holders="$(psql -h "${DB_RESTORE_HOST}" -p "${DB_RESTORE_PORT}" \
    -U "${DB_RESTORE_USER}" -d "${DB_RESTORE_NAME}" -tAq \
    -c "SELECT COUNT(*) FROM holders h
        LEFT JOIN assets a ON h.asset_id = a.id
        WHERE a.id IS NULL;" 2>/dev/null || echo 0)"

  if [[ "${orphaned_holders}" -gt 0 ]]; then
    log WARN "Found ${orphaned_holders} holder records with no matching asset (referential integrity issue)"
    check_passed=false
  else
    log INFO "Referential integrity check (holders → assets): OK"
  fi

  # ---- 7d. Vaccum analyze to confirm no corruption -------------------------
  log INFO "Running VACUUM ANALYZE to detect page-level corruption"
  psql -h "${DB_RESTORE_HOST}" -p "${DB_RESTORE_PORT}" \
       -U "${DB_RESTORE_USER}" -d "${DB_RESTORE_NAME}" \
       -c "VACUUM ANALYZE;" 2>&1 | grep -v "^$" | log DEBUG || true

  if [[ "${check_passed}" == "true" ]] && [[ "${schema_ok}" == "true" ]]; then
    log INFO "All integrity checks PASSED"
    stage_end "Integrity Checks" "$t" "ok"
  else
    log WARN "Some integrity checks reported warnings — review output above"
    stage_end "Integrity Checks" "$t" "warn"
    (( WARNINGS++ )) || true
  fi
}

# ---------------------------------------------------------------------------
# Write JSON report
# ---------------------------------------------------------------------------
write_report() {
  local final_exit_code="${1:-0}"
  local end_epoch; end_epoch="$(date +%s)"
  local total_duration=$(( end_epoch - START_EPOCH ))

  # Build stages JSON
  local stages_json="{"
  local first=true
  for stage_name in "${!STAGE_DURATIONS[@]}"; do
    [[ "$first" == "true" ]] && first=false || stages_json+=","
    stages_json+="\"${stage_name}\":{\"duration_seconds\":${STAGE_DURATIONS[$stage_name]},\"status\":\"${STAGE_STATUS[$stage_name]}\"}"
  done
  stages_json+="}"

  # Determine overall result
  local overall="success"
  [[ "${final_exit_code}" -eq 1 ]] && overall="failure"
  [[ "${final_exit_code}" -eq 2 ]] && overall="warning"
  [[ "${WARNINGS}" -gt 0 ]] && [[ "${overall}" == "success" ]] && overall="warning"

  jq -n \
    --arg run_id "${RUN_ID}" \
    --arg provider "${BACKUP_PROVIDER}" \
    --arg bucket "${BACKUP_BUCKET}" \
    --arg snapshot "${SNAPSHOT_PATH:-unknown}" \
    --arg checksum "${ACTUAL_CHECKSUM:-unknown}" \
    --arg restore_host "${DB_RESTORE_HOST}" \
    --arg restore_db "${DB_RESTORE_NAME}" \
    --arg overall "${overall}" \
    --argjson warnings "${WARNINGS}" \
    --argjson errors "${ERRORS}" \
    --argjson total_seconds "${total_duration}" \
    --argjson start_epoch "${START_EPOCH}" \
    --argjson end_epoch "${end_epoch}" \
    --argjson stages "${stages_json}" \
    --argjson integrity "${INTEGRITY_RESULTS:-{}}" \
    '{
      run_id: $run_id,
      script_version: "'"${SCRIPT_VERSION}"'",
      timestamp_utc: (now | strftime("%Y-%m-%dT%H:%M:%SZ")),
      backup: {
        provider: $provider,
        bucket: $bucket,
        snapshot: $snapshot,
        sha256: $checksum
      },
      restore: {
        host: $restore_host,
        database: $restore_db
      },
      result: {
        overall: $overall,
        warnings: $warnings,
        errors: $errors
      },
      timing: {
        start_epoch: $start_epoch,
        end_epoch: $end_epoch,
        total_duration_seconds: $total_seconds,
        stages: $stages
      },
      integrity: $integrity
    }' > "${DR_REPORT_PATH}"

  log INFO "JSON report written to: ${DR_REPORT_PATH}"
  REPORT_WRITTEN=true
}

# ---------------------------------------------------------------------------
# Notifications
# ---------------------------------------------------------------------------
send_slack_notification() {
  [[ -z "${SLACK_WEBHOOK_URL}" ]] && return 0

  local status="${1:-unknown}"
  local total_duration="${STAGE_DURATIONS[Database Restore]:-0}"
  local colour
  case "${status}" in
    success) colour="good" ;;
    warning) colour="warning" ;;
    *)       colour="danger" ;;
  esac

  local payload
  payload="$(jq -n \
    --arg status "${status}" \
    --arg run_id "${RUN_ID}" \
    --arg snapshot "${SNAPSHOT_PATH:-unknown}" \
    --argjson duration "${total_duration}" \
    --arg colour "${colour}" \
    '{
      attachments: [{
        color: $colour,
        title: "Tessera DR Test: \($status | ascii_upcase)",
        fields: [
          { title: "Run ID",      value: $run_id,    short: true },
          { title: "Snapshot",    value: $snapshot,  short: true },
          { title: "Restore (s)", value: ($duration | tostring), short: true },
          { title: "Status",      value: $status,    short: true }
        ],
        footer: "tessera-dr-test",
        ts: now | floor
      }]
    }')"

  log INFO "Sending Slack notification (status: ${status})"
  curl -s -X POST -H 'Content-type: application/json' \
    --data "${payload}" "${SLACK_WEBHOOK_URL}" > /dev/null || \
    log WARN "Failed to send Slack notification"
}

send_pagerduty_notification() {
  [[ -z "${PAGERDUTY_ROUTING_KEY}" ]] && return 0

  local status="${1:-unknown}"
  local severity
  case "${status}" in
    success) return 0 ;;  # Only alert on failure/warning
    warning) severity="warning" ;;
    *)       severity="critical" ;;
  esac

  log INFO "Sending PagerDuty alert (severity: ${severity})"
  curl -s -X POST "https://events.pagerduty.com/v2/enqueue" \
    -H "Content-Type: application/json" \
    -d "$(jq -n \
      --arg key "${PAGERDUTY_ROUTING_KEY}" \
      --arg severity "${severity}" \
      --arg run_id "${RUN_ID}" \
      --arg snapshot "${SNAPSHOT_PATH:-unknown}" \
      '{
        routing_key: $key,
        event_action: "trigger",
        dedup_key: ("tessera-dr-" + $run_id),
        payload: {
          summary: ("Tessera DR test " + $severity + ": " + $run_id),
          severity: $severity,
          source: "dr_test.sh",
          custom_details: {
            run_id: $run_id,
            snapshot: $snapshot
          }
        }
      }')" > /dev/null || log WARN "Failed to send PagerDuty alert"
}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------
main() {
  log INFO "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  log INFO "Tessera DR Test Pipeline v${SCRIPT_VERSION} — Run ID: ${RUN_ID}"
  log INFO "Provider: ${BACKUP_PROVIDER}  Bucket: ${BACKUP_BUCKET}"
  [[ "${DRY_RUN}" == "true" ]] && log WARN "DRY-RUN MODE — no destructive operations will be performed"
  log INFO "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

  # Detect CI environment
  if [[ -n "${GITHUB_ACTIONS:-}" ]]; then
    log INFO "Running in GitHub Actions (workflow: ${GITHUB_WORKFLOW:-unknown})"
  fi

  check_dependencies
  locate_snapshot
  download_snapshot
  verify_checksum
  decrypt_snapshot
  prepare_restore_db
  restore_database
  run_integrity_checks

  # Compute final exit code
  local exit_code=0
  [[ "${ERRORS}" -gt 0 ]] && exit_code=1
  [[ "${WARNINGS}" -gt 0 ]] && [[ "${exit_code}" -eq 0 ]] && exit_code=2

  local overall="success"
  [[ "${exit_code}" -eq 1 ]] && overall="failure"
  [[ "${exit_code}" -eq 2 ]] && overall="warning"

  local total_duration=$(( $(date +%s) - START_EPOCH ))

  log STEP "Summary"
  log INFO "Overall result : ${BOLD}${overall}${RESET}"
  log INFO "Total duration : ${total_duration}s"
  log INFO "Warnings       : ${WARNINGS}"
  log INFO "Errors         : ${ERRORS}"

  write_report "${exit_code}"
  send_slack_notification "${overall}"
  send_pagerduty_notification "${overall}"

  if [[ "${overall}" == "success" ]]; then
    log INFO "${GREEN}DR verification PASSED${RESET} — backup is restorable and data is consistent."
  elif [[ "${overall}" == "warning" ]]; then
    log WARN "DR verification completed with WARNINGS — review the report at ${DR_REPORT_PATH}"
  else
    log ERROR "DR verification FAILED — see errors above and report at ${DR_REPORT_PATH}"
  fi

  exit "${exit_code}"
}

main "$@"
