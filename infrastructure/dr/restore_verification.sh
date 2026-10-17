#!/usr/bin/env bash
# Daily disaster-recovery entry point (issue #310).
#
# Restores the latest encrypted S3/GCS Postgres backup into an isolated
# sandbox database, asserts checksums and record counts, and alerts the SRE
# team (Slack / PagerDuty) when restoration fails or data is corrupt. All of
# that logic lives in infrastructure/scripts/dr_test.sh (issue #140); this is
# the entry point invoked by the daily DR workflow.
#
# Exit codes: 0 pass, 1 failure (alerts sent), 2 warnings.
set -euo pipefail

exec "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/../scripts/dr_test.sh" "$@"
