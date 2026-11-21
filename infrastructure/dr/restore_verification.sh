#!/usr/bin/env bash
# Disaster-recovery entry point (issues #310, #393).
#
# Without flags this is the daily backup restore verification: the latest
# encrypted S3/GCS Postgres backup is restored into an isolated sandbox,
# checksums and record counts are asserted and SRE is alerted on failure. All
# of that logic lives in infrastructure/scripts/dr_test.sh (issue #140).
#
# With --drill this entry point runs the automated weekly DR drill (issue #393):
# scale up the warm Fargate standby, restore the newest WAL from pgBackRest,
# health-check the read-only API, run the API test suite, measure RTO/RPO and
# report to PagerDuty. That is what .github/workflows/dr_drill.yml invokes.
#
# Exit codes: 0 pass, 1 failure (alerts sent), 2 warnings/objective missed.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

if [[ "${1:-}" == "--drill" ]]; then
  shift
  exec "${SCRIPT_DIR}/dr_drill.sh" "$@"
fi

exec "${SCRIPT_DIR}/../scripts/dr_test.sh" "$@"
