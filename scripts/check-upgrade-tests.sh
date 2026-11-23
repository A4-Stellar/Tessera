#!/usr/bin/env bash
set -euo pipefail

# CI Gate: Contract Storage Migration & Upgrade Test Verification (Issue #449 / #66)
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
node "$DIR/scripts/check-upgrade-tests.cjs"
