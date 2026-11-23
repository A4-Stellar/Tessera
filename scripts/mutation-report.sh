#!/usr/bin/env bash
set -euo pipefail

# Mutation Testing Runner & Report Generator (Issue #448 / #65)
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
node "$DIR/scripts/run-mutation-tests.cjs"
