#!/usr/bin/env bash
# Continuous WAL archive health check for Tessera DR (issue #393).
#
# Called by the systemd timer / K8s CronJob every minute on the primary:
#   1. verify both S3 and GCS repositories are reachable,
#   2. force a WAL segment switch so idle periods still produce an archived
#      segment (Postgres `archive_timeout = 300` is the backstop),
#   3. assert the newest archived WAL is younger than the 5-minute RPO target.
#
# Exit codes: 0 healthy, 1 RPO breach / repo failure.
set -euo pipefail

STANZA="${PGBACKREST_STANZA:-prod}"
RPO_SECONDS="${DR_RPO_SECONDS:-300}"
PSQL_DSN="${RWA_DATABASE_URL:-postgresql://postgres@localhost:5432/postgres}"

log() { printf '[wal-archive] %s %s\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ')" "$*"; }

command -v pgbackrest >/dev/null || { log "pgbackrest is not installed"; exit 1; }
command -v psql >/dev/null || { log "psql is not installed"; exit 1; }

for repo in 1 2; do
  log "checking repository ${repo}"
  pgbackrest --stanza="${STANZA}" --repo="${repo}" check || {
    log "repository ${repo} is unavailable"
    exit 1
  }
done

log "switching WAL segment"
psql "${PSQL_DSN}" -Atqc 'SELECT pg_switch_wal();' >/dev/null

info="$(pgbackrest --stanza="${STANZA}" --output=json info)"
last_wal_epoch="$(printf '%s' "${info}" | jq -r '
  [.[] | .backup[]?.timestamp.stop // empty] | max // 0')"
if [[ "${last_wal_epoch}" -eq 0 ]]; then
  log "no completed backup/WAL found in either repository"
  exit 1
fi

age=$(( $(date +%s) - last_wal_epoch ))
log "newest archived WAL is ${age}s old (RPO target ${RPO_SECONDS}s)"
if (( age > RPO_SECONDS )); then
  log "RPO BREACH: archived WAL is older than ${RPO_SECONDS}s"
  exit 1
fi
log "WAL archiving healthy"
