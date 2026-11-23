#!/usr/bin/env bash
# =============================================================================
# test_check_wasm_cache.sh — self-contained tests for check-wasm-cache.sh
# =============================================================================
# No aws/gsutil and no real cargo build are required: cargo is stubbed on PATH
# and the cache backend is exercised purely through the local (target/) cache.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CHECK="$SCRIPT_DIR/check-wasm-cache.sh"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

PASS=0
FAIL=0
ok() { printf 'PASS: %s\n' "$1"; PASS=$((PASS + 1)); }
bad() { printf 'FAIL: %s\n' "$1"; FAIL=$((FAIL + 1)); }
assert_eq() {
  if [[ "$1" == "$2" ]]; then ok "$3"; else bad "$3 (expected '$2', got '$1')"; fi
}

# Run the script in selftest mode; it prints only the content-hash cache key.
hash_of() {
  CONTRACTS_DIR="$1" WASM_CACHE_SELFTEST=1 bash "$CHECK"
}

# -----------------------------------------------------------------------------
# 1. Hash determinism: two identical trees (different dirs, different file
#    creation order) must produce the same key; editing a file changes it.
# -----------------------------------------------------------------------------
A="$TMP/tree-a"
B="$TMP/tree-b"

mkdir -p "$A/src" "$A/ct/src"
printf '[package]\nname = "a"\nversion = "0.1.0"\n' >"$A/Cargo.toml"
printf 'version = 3\n' >"$A/Cargo.lock"
printf '[toolchain]\nchannel = "1.81.0"\n' >"$A/rust-toolchain.toml"
printf 'pub fn alpha() {}\n' >"$A/src/lib.rs"
printf 'pub fn contract_a() {}\n' >"$A/ct/src/lib.rs"

# Same contents, created in a different order and in a different directory.
mkdir -p "$B/ct/src" "$B/src"
printf 'pub fn contract_a() {}\n' >"$B/ct/src/lib.rs"
printf 'pub fn alpha() {}\n' >"$B/src/lib.rs"
printf '[toolchain]\nchannel = "1.81.0"\n' >"$B/rust-toolchain.toml"
printf 'version = 3\n' >"$B/Cargo.lock"
printf '[package]\nname = "a"\nversion = "0.1.0"\n' >"$B/Cargo.toml"

K1="$(hash_of "$A")"
K2="$(hash_of "$B")"
assert_eq "$K1" "$K2" "identical trees in different dirs/order hash equally"

if [[ "$K1" =~ ^soroban-wasm-[0-9a-f]{64}$ ]]; then
  ok "key format is soroban-wasm-<64 hex>"
else
  bad "key format is soroban-wasm-<64 hex> (got '$K1')"
fi

printf 'pub fn alpha() { let _changed = 1; }\n' >"$B/src/lib.rs"
K3="$(hash_of "$B")"
if [[ "$K3" != "$K1" ]]; then ok "editing a source file changes the hash"; else bad "editing a source file changes the hash"; fi

# A different CARGO_FLAGS must also change the hash.
K4="$(CONTRACTS_DIR="$A" CARGO_FLAGS="--release --target wasm32-unknown-unknown --locked" WASM_CACHE_SELFTEST=1 bash "$CHECK")"
if [[ "$K4" != "$K1" ]]; then ok "CARGO_FLAGS are part of the hash"; else bad "CARGO_FLAGS are part of the hash"; fi

# -----------------------------------------------------------------------------
# 2. Fake cargo on PATH: records that it was called and emits a dummy .wasm.
# -----------------------------------------------------------------------------
BIN="$TMP/bin"
mkdir -p "$BIN"
cat >"$BIN/cargo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf 'cargo-called\n' >>"${CARGO_SENTINEL:?}"
out="${CARGO_TARGET_DIR:-target}/wasm32-unknown-unknown/release"
mkdir -p "$out"
printf 'FAKE-WASM-BYTES' >"$out/dummy.wasm"
exit 0
EOF
chmod +x "$BIN/cargo"
PATH="$BIN:$PATH"
export PATH

KEY="$K1"

# --- HIT: pre-seed the local cache, assert cargo is NOT called ---------------
T_HIT="$TMP/target-hit"
CACHE_HIT="$T_HIT/.wasm-cache/$KEY"
mkdir -p "$CACHE_HIT"
printf 'FAKE-WASM-BYTES' >"$CACHE_HIT/dummy.wasm"
SENTINEL_HIT="$TMP/sentinel.hit"
SUMMARY_HIT="$TMP/summary.hit.md"
METRICS_HIT="$TMP/metrics.hit.json"
: >"$SUMMARY_HIT"

set +e
CARGO_TARGET_DIR="$T_HIT" CONTRACTS_DIR="$A" CACHE_BUCKET="" \
  WASM_OUT_DIR="$T_HIT/wasm32-unknown-unknown/release" \
  GITHUB_STEP_SUMMARY="$SUMMARY_HIT" METRICS_FILE="$METRICS_HIT" \
  CARGO_SENTINEL="$SENTINEL_HIT" \
  bash "$CHECK" >"$TMP/hit.log" 2>&1
HIT_RC=$?
set -e

if [[ "$HIT_RC" -eq 0 ]]; then ok "HIT run exits 0"; else bad "HIT run exited $HIT_RC"; cat "$TMP/hit.log"; fi
if [[ ! -e "$SENTINEL_HIT" ]]; then ok "HIT does not call cargo"; else bad "HIT called cargo (sentinel exists)"; fi
if grep -q 'WASM cache HIT' "$SUMMARY_HIT"; then ok "HIT summary line written"; else bad "HIT summary line written"; fi
if [[ -f "$T_HIT/wasm32-unknown-unknown/release/dummy.wasm" ]]; then ok "HIT restores wasm into WASM_OUT_DIR"; else bad "HIT restores wasm into WASM_OUT_DIR"; fi
if grep -q '"result":"hit"' "$METRICS_HIT"; then ok "HIT metrics record result=hit"; else bad "HIT metrics record result=hit"; fi
if grep -q '"hit_rate":100' "$METRICS_HIT"; then ok "HIT hit rate is 100%"; else bad "HIT hit rate is 100%"; fi

# --- MISS: empty cache, assert cargo IS called and cache is seeded ----------
T_MISS="$TMP/target-miss"
SENTINEL_MISS="$TMP/sentinel.miss"
SUMMARY_MISS="$TMP/summary.miss.md"
METRICS_MISS="$TMP/metrics.miss.json"
: >"$SUMMARY_MISS"

set +e
CARGO_TARGET_DIR="$T_MISS" CONTRACTS_DIR="$A" CACHE_BUCKET="" \
  WASM_OUT_DIR="$T_MISS/wasm32-unknown-unknown/release" \
  GITHUB_STEP_SUMMARY="$SUMMARY_MISS" METRICS_FILE="$METRICS_MISS" \
  CARGO_SENTINEL="$SENTINEL_MISS" \
  bash "$CHECK" >"$TMP/miss.log" 2>&1
MISS_RC=$?
set -e

if [[ "$MISS_RC" -eq 0 ]]; then ok "MISS run exits 0"; else bad "MISS run exited $MISS_RC"; cat "$TMP/miss.log"; fi
if [[ -e "$SENTINEL_MISS" ]]; then ok "MISS calls cargo"; else bad "MISS calls cargo (sentinel missing)"; fi
if grep -q 'WASM cache MISS' "$SUMMARY_MISS"; then ok "MISS summary line written"; else bad "MISS summary line written"; fi
if grep -q '"result":"miss"' "$METRICS_MISS"; then ok "MISS metrics record result=miss"; else bad "MISS metrics record result=miss"; fi
if [[ -f "$T_MISS/.wasm-cache/$KEY/dummy.wasm" ]]; then ok "MISS seeds local cache with built wasm"; else bad "MISS seeds local cache with built wasm"; fi
if grep -q '"hit_rate":0' "$METRICS_MISS"; then ok "MISS hit rate is 0%"; else bad "MISS hit rate is 0%"; fi

# --- Reproducibility: two independent builds must be byte-identical ---------
T_REPRO="$TMP/target-repro"
SENTINEL_REPRO="$TMP/sentinel.repro"
SUMMARY_REPRO="$TMP/summary.repro.md"
METRICS_REPRO="$TMP/metrics.repro.json"
: >"$SUMMARY_REPRO"

set +e
CARGO_TARGET_DIR="$T_REPRO" CONTRACTS_DIR="$A" CACHE_BUCKET="" \
  WASM_OUT_DIR="$T_REPRO/wasm32-unknown-unknown/release" \
  WASM_REPRODUCIBILITY_CHECK=1 \
  GITHUB_STEP_SUMMARY="$SUMMARY_REPRO" METRICS_FILE="$METRICS_REPRO" \
  CARGO_SENTINEL="$SENTINEL_REPRO" \
  bash "$CHECK" >"$TMP/repro.log" 2>&1
REPRO_RC=$?
set -e

if [[ "$REPRO_RC" -eq 0 ]]; then ok "reproducibility check exits 0 on identical builds"; else bad "reproducibility check exited $REPRO_RC"; cat "$TMP/repro.log"; fi
if grep -q 'Reproducibility PASS' "$SUMMARY_REPRO"; then ok "reproducibility PASS line written"; else bad "reproducibility PASS line written"; fi

printf '\n%d passed, %d failed\n' "$PASS" "$FAIL"
if [[ "$FAIL" -gt 0 ]]; then exit 1; fi
exit 0
