#!/usr/bin/env bash
#
# Runs every TLA+/TLC model check for the compliance/token invariants.
#
# Usage:
#   ./verify.sh                 # download tla2tools.jar if needed, then verify
#   TLC_JAR=/path/to/jar ./verify.sh
#
# Exit codes:
#   0  every expected model behaved as expected
#   1  a model that was supposed to PASS failed  (real regression)
#   2  the refutation model was supposed to FAIL but passed  (the naive
#      "positive balance implies allowlisted" claim became true, which means
#      either the system changed or the model drifted from the Rust -- either
#      way README.md is now wrong and must be updated)
#
set -uo pipefail

cd "$(dirname "$0")"

TLA_VERSION="1.7.4"
TLA_URL="https://github.com/tlaplus/tlaplus/releases/download/v${TLA_VERSION}/tla2tools.jar"
TLA_JAR="${TLC_JAR:-${TMPDIR:-/tmp}/tla2tools-${TLA_VERSION}.jar}"
SPEC="ComplianceSupply.tla"

if [ ! -f "$TLA_JAR" ]; then
  echo "==> fetching tla2tools ${TLA_VERSION}"
  curl -fsSL "$TLA_URL" -o "$TLA_JAR" || {
    echo "error: could not download $TLA_URL" >&2
    exit 1
  }
fi

WORKERS="${TLC_WORKERS:-auto}"
JAVA_OPTS="${TLC_JAVA_OPTS:--XX:+UseParallelGC}"

run() {
  # shellcheck disable=SC2086
  # 2>&1 so TLC's coverage report (which it writes to stderr) is captured once
  # instead of appearing twice.
  java $JAVA_OPTS -cp "$TLA_JAR" tlc2.TLC -workers "$WORKERS" -deadlock "$@" "$SPEC" 2>&1
}

rc=0

# Actions that MUST be enabled at least once for the invariants above to mean
# anything. A property over an action that is never enabled is vacuously true,
# and that is not a hypothetical: `Transfer` was unreachable in the first draft
# of the model because `add_to_allowlist` had been mistranscribed with the guard
# `subject = admin`. See the "Two bugs this found" section of README.md.
REQUIRED_ACTIONS="Transfer Mint Burn Clawback InitializeToken AddToAllowlist Suspend Remove BlockJurisdiction SelfTransfer"

echo
echo "=============================================================="
echo " 1/2  Gate + supply invariants  (Gate.cfg)"
echo "      11 invariants, expected to PASS"
echo "      (also collecting action coverage to prove non-vacuity)"
echo "=============================================================="
out="$(run -coverage 1 -config Gate.cfg)"
echo "$out"
if echo "$out" | grep -q "No error has been found"; then
  echo "PASS  $(echo "$out" | grep -o '[0-9]* distinct states found' | tail -1)"
else
  echo "FAIL  expected 'No error has been found', got a violation or crash" >&2
  rc=1
fi

echo
echo "-- non-vacuity: every credit/revoke path must actually be taken --"
# TLC's -coverage output is one line per action:
#
#     <Transfer line 309, col 1 to line 309, col 19 of module ComplianceSupply>: 55:19200
#                                                                              ^  ^
#                                       distinct NEW successor states .......  |  |
#                                       times the action was TAKEN .........  |  |
#
# The right-hand number is the one to test. The left-hand number is the count of
# brand-new states the action discovered, which is legitimately 0 for an action
# whose successors are always already reachable by another path -- SelfTransfer
# (a no-op) and Burn both show 0 there while being taken hundreds of thousands
# of times. Testing the wrong column would have declared both of them dead.
for action in $REQUIRED_ACTIONS; do
  taken="$(echo "$out" \
    | grep -E "^<${action} line [0-9]+, col [0-9]+ to line [0-9]+, col [0-9]+ of module [A-Za-z]+>: [0-9]+:[0-9]+\]?$" \
    | tail -1 | sed -E 's/.*:([0-9]+)\]?$/\1/')"
  if [ -z "$taken" ]; then
    echo "FAIL  $action: no coverage line found (was the action renamed?)" >&2
    rc=1
  elif [ "$taken" -eq 0 ]; then
    echo "FAIL  $action is never taken -- invariants over it may be vacuous" >&2
    rc=1
  else
    echo "ok    $action taken $taken times"
  fi
done

echo
echo "=============================================================="
echo " 2/2  Naive steady-state claim  (Refutation.cfg)"
echo "      PositiveBalanceImpliesAllowed, expected to FAIL"
echo "=============================================================="
out="$(run -config Refutation.cfg)"
echo "$out"
if echo "$out" | grep -q "Invariant PositiveBalanceImpliesAllowed is violated"; then
  echo "PASS  refutation reproduced (a positive balance can outlive its allowlist entry)"
elif echo "$out" | grep -q "No error has been found"; then
  echo "FAIL  the naive claim now HOLDS. The system or the model changed;" >&2
  echo "      README.md section 'The claim that is false' is stale." >&2
  rc=2
else
  echo "FAIL  expected an invariant violation, TLC crashed or errored" >&2
  rc=1
fi

echo
if [ "$rc" -eq 0 ]; then
  echo "==> all model checks behaved as expected"
else
  echo "==> model check FAILED (rc=$rc)" >&2
fi
exit "$rc"
