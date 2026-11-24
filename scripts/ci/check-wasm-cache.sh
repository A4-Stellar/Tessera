#!/usr/bin/env bash
# =============================================================================
# check-wasm-cache.sh — content-addressed WASM build cache for Soroban contracts
# =============================================================================
#
# USAGE
#   bash scripts/ci/check-wasm-cache.sh [--help]
#
# Computes a deterministic content hash over the contract sources, the rustc
# version and the exact CARGO_FLAGS. The resulting cache key is
#   <CACHE_KEY_PREFIX>-<64-hex sha256>      (default: soroban-wasm-<hash>)
#
# If artifacts for that key exist in the local cache (or in CACHE_BUCKET, when
# configured) they are restored and compilation is skipped. Otherwise the
# contracts are compiled from source and the resulting *.wasm files are pushed
# to the local cache and, when configured, to the S3/GCS bucket with an expiry
# tag. A hit/miss history log is kept so the hit rate can be reported.
#
# All inputs are environment variables (see defaults below). Set
# WASM_CACHE_SELFTEST=1 to compute and print only the key (used by the tests).
#
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: bash scripts/ci/check-wasm-cache.sh [--help]

Content-addressed WASM build cache for Soroban contracts.

Environment variables (defaults in parentheses):
  CONTRACTS_DIR              (contracts)                 Source root (Cargo.toml workspace)
  CACHE_BUCKET               (empty = local only)        s3://bucket/prefix or gs://bucket/prefix
  CACHE_KEY_PREFIX           (soroban-wasm)              Cache key prefix; final key = <prefix>-<hash>
  CARGO_FLAGS                (--release --target wasm32-unknown-unknown)
  RETENTION_DAYS             (30)                        Days until a cached object expires
  GITHUB_STEP_SUMMARY        (empty)                     File to append markdown summary lines to
  METRICS_FILE               (.wasm-cache-metrics.json)  JSON metrics output path
  WASM_OUT_DIR               (<CARGO_TARGET_DIR>/wasm32-unknown-unknown/release)
  CARGO_TARGET_DIR           (<CONTRACTS_DIR>/target)    Stable cargo target directory
  WASM_REPRODUCIBILITY_CHECK (0)                         Set 1 to verify byte-identical rebuilds
  WASM_CACHE_SELFTEST        (0)                         Set 1 to only print the cache key and exit

The local cache and history live under CARGO_TARGET_DIR (a target/ directory,
which is gitignored): <CARGO_TARGET_DIR>/.wasm-cache and
<CARGO_TARGET_DIR>/.wasm-cache-history.jsonl.
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --help | -h)
      usage
      exit 0
      ;;
    *)
      echo "check-wasm-cache.sh: unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

# -----------------------------------------------------------------------------
# Configuration (all overridable via environment)
# -----------------------------------------------------------------------------
CONTRACTS_DIR="${CONTRACTS_DIR:-contracts}"
CACHE_BUCKET="${CACHE_BUCKET:-}"
CACHE_KEY_PREFIX="${CACHE_KEY_PREFIX:-soroban-wasm}"
CARGO_FLAGS="${CARGO_FLAGS:---release --target wasm32-unknown-unknown}"
RETENTION_DAYS="${RETENTION_DAYS:-30}"
GITHUB_STEP_SUMMARY="${GITHUB_STEP_SUMMARY:-}"
METRICS_FILE="${METRICS_FILE:-.wasm-cache-metrics.json}"
CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-${CONTRACTS_DIR}/target}"
# Default is the wasm release directory inside the stable target dir
# (with the defaults above: contracts/target/wasm32-unknown-unknown/release,
# i.e. target/wasm32-unknown-unknown/release relative to the target dir).
WASM_OUT_DIR="${WASM_OUT_DIR:-${CARGO_TARGET_DIR}/wasm32-unknown-unknown/release}"
WASM_REPRODUCIBILITY_CHECK="${WASM_REPRODUCIBILITY_CHECK:-0}"

# Stable, reproducible builds: a fixed SOURCE_DATE_EPOCH removes timestamps
# from wasm metadata. A pinned toolchain (contracts/rust-toolchain.toml) is
# also required for two independent runners to agree byte-for-byte.
export SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-1}"

LOCAL_CACHE_ROOT="${CARGO_TARGET_DIR}/.wasm-cache"
HISTORY_FILE="${CARGO_TARGET_DIR}/.wasm-cache-history.jsonl"

if [[ ! -d "$CONTRACTS_DIR" ]]; then
  echo "check-wasm-cache.sh: CONTRACTS_DIR '$CONTRACTS_DIR' does not exist" >&2
  exit 1
fi

# -----------------------------------------------------------------------------
# Helpers
# -----------------------------------------------------------------------------
sha256_file() {
  local path="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$path" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$path" | awk '{print $1}'
  else
    echo "check-wasm-cache.sh: need sha256sum or shasum to hash sources" >&2
    exit 1
  fi
}

# Print *.wasm files under a directory as paths relative to that directory,
# sorted byte-wise (LC_ALL=C) so results do not depend on filesystem order.
list_wasm_rel() {
  local root="$1"
  [[ -d "$root" ]] || return 0
  (
    cd "$root"
    find . -type f -name '*.wasm' -print 2>/dev/null \
      | sed 's#^\./##' \
      | LC_ALL=C sort
  )
}

# Compute the content hash and the cache key. The hash covers:
#   1. every contract source file (.rs, Cargo.toml, Cargo.lock,
#      rust-toolchain.toml) as a (relative path, size, contents) tuple,
#      sorted by relative path — independent of filesystem order and of the
#      absolute location of the source tree;
#   2. the rustc version string;
#   3. the exact CARGO_FLAGS.
# Sets the globals HASH and KEY and prints the key.
compute_hash() {
  local tmp rel size
  tmp="$(mktemp)"
  trap 'rm -f "$tmp"' RETURN

  {
    printf 'rustc: %s\n' "$(rustc --version 2>/dev/null || echo 'unknown')"
    printf 'cargo_flags: %s\n' "$CARGO_FLAGS"
    printf '%s\n' '--- files ---'
  } >"$tmp"

  (
    cd "$CONTRACTS_DIR"
    find . -type f \( \
      -name '*.rs' \
      -o -name 'Cargo.toml' \
      -o -name 'Cargo.lock' \
      -o -name 'rust-toolchain.toml' \
    \) -print 2>/dev/null
  ) | LC_ALL=C sort | while IFS= read -r rel; do
    rel="${rel#./}"
    size="$(wc -c <"$CONTRACTS_DIR/$rel" | tr -d '[:space:]')"
    printf 'FILE %s\nSIZE %s\n' "$rel" "$size"
    cat "$CONTRACTS_DIR/$rel"
    printf '\n'
  done >>"$tmp"

  HASH="$(sha256_file "$tmp")"
  rm -f "$tmp"
  trap - RETURN

  if [[ ! "$HASH" =~ ^[0-9a-f]{64}$ ]]; then
    echo "check-wasm-cache.sh: failed to compute a 64-hex hash" >&2
    exit 1
  fi
  KEY="${CACHE_KEY_PREFIX}-${HASH}"
  printf '%s\n' "$KEY"
}

summarize() {
  # Append a markdown line to the GitHub step summary when requested.
  if [[ -n "$GITHUB_STEP_SUMMARY" ]]; then
    printf '%s\n' "$1" >>"$GITHUB_STEP_SUMMARY"
  fi
}

expiry_date() {
  # Date string written into the object metadata/tag for 30-day expiry.
  if date -d "+${RETENTION_DAYS} days" +%Y-%m-%d >/dev/null 2>&1; then
    date -d "+${RETENTION_DAYS} days" +%Y-%m-%d
  else
    date -v+"${RETENTION_DAYS}d" +%Y-%m-%d
  fi
}

# -----------------------------------------------------------------------------
# Cache backend. These two functions are the only places that talk to object
# storage, so they are deliberately small and easy to stub in tests.
# -----------------------------------------------------------------------------
cache_fetch() {
  # cache_fetch <key> <local-dest-dir>
  # Returns 0 on a hit (dest contains at least one .wasm), 1 otherwise.
  local key="$1" dest="$2"

  # Fast path: the local cache (restored by actions/cache or a previous run).
  if [[ -d "$dest" ]] && compgen -G "$dest/*.wasm" >/dev/null; then
    return 0
  fi

  [[ -n "$CACHE_BUCKET" ]] || return 1
  mkdir -p "$dest"

  case "$CACHE_BUCKET" in
    s3://*)
      command -v aws >/dev/null 2>&1 || return 1
      aws s3 cp "$CACHE_BUCKET/$key/" "$dest/" --recursive >/dev/null 2>&1 || return 1
      ;;
    gs://*)
      command -v gsutil >/dev/null 2>&1 || return 1
      gsutil -m cp -r "$CACHE_BUCKET/$key/*" "$dest/" >/dev/null 2>&1 || return 1
      ;;
    *) return 1 ;;
  esac

  [[ -d "$dest" ]] && compgen -G "$dest/*.wasm" >/dev/null
}

cache_push() {
  # cache_push <key> <wasm-source-dir> <local-dest-dir>
  # Mirrors wasm into the local cache and, when a bucket is configured, uploads
  # it with an expiry-date tag. A bucket lifecycle rule (or the expiry-date tag)
  # enforces the RETENTION_DAYS expiry.
  local key="$1" src="$2" dest="$3"
  local rel copied=0

  mkdir -p "$dest"
  while IFS= read -r rel; do
    [[ -z "$rel" ]] && continue
    if cp -f "$src/$rel" "$dest/$rel"; then
      copied=$((copied + 1))
    fi
  done < <(list_wasm_rel "$src")

  if [[ -n "$CACHE_BUCKET" && "$copied" -gt 0 ]]; then
    local expiry
    expiry="$(expiry_date)"
    case "$CACHE_BUCKET" in
      s3://*)
        if command -v aws >/dev/null 2>&1; then
          aws s3 cp "$dest/" "$CACHE_BUCKET/$key/" --recursive \
            --metadata "expiry-date=${expiry}" \
            --tagging "expiry-date=${expiry}" >/dev/null 2>&1 || true
        fi
        ;;
      gs://*)
        if command -v gsutil >/dev/null 2>&1; then
          gsutil -m cp -r "$dest/." "$CACHE_BUCKET/$key/" \
            -h "x-goog-meta-expiry-date:${expiry}" >/dev/null 2>&1 || true
        fi
        ;;
    esac
  fi
  return 0
}

copy_cache_to_out() {
  local src="$1" out="$2" rel
  mkdir -p "$out"
  while IFS= read -r rel; do
    [[ -z "$rel" ]] && continue
    cp -f "$src/$rel" "$out/$rel"
  done < <(list_wasm_rel "$src")
}

# -----------------------------------------------------------------------------
# Build / reproducibility helpers
# -----------------------------------------------------------------------------
run_build() {
  # run_build <cargo-target-dir>
  local target_dir="$1"
  mkdir -p "$target_dir"
  # shellcheck disable=SC2086  # CARGO_FLAGS must be word-split into arguments.
  CARGO_TARGET_DIR="$target_dir" SOURCE_DATE_EPOCH="$SOURCE_DATE_EPOCH" \
    cargo build $CARGO_FLAGS --locked --manifest-path "$CONTRACTS_DIR/Cargo.toml"
}

wasm_out_for() {
  printf '%s/wasm32-unknown-unknown/release' "$1"
}

compare_wasm_dirs() {
  # compare_wasm_dirs <dir-a> <dir-b> <label> — fail if any .wasm differs.
  local dir_a="$1" dir_b="$2" label="$3"
  local list_a list_b rel
  list_a="$(list_wasm_rel "$dir_a")"
  list_b="$(list_wasm_rel "$dir_b")"
  if [[ "$list_a" != "$list_b" ]]; then
    echo "check-wasm-cache.sh: reproducibility FAILED ($label): wasm file sets differ" >&2
    echo "  A: ${list_a//$'\n'/, }" >&2
    echo "  B: ${list_b//$'\n'/, }" >&2
    exit 1
  fi
  [[ -z "$list_a" ]] && return 0
  while IFS= read -r rel; do
    [[ -z "$rel" ]] && continue
    if ! cmp -s "$dir_a/$rel" "$dir_b/$rel"; then
      echo "check-wasm-cache.sh: reproducibility FAILED ($label): '$rel' differs byte-for-byte" >&2
      echo "  Compare: cmp '$dir_a/$rel' '$dir_b/$rel'" >&2
      exit 1
    fi
  done <<<"$list_a"
}

reproducibility_check() {
  # On a fresh build: two clean, independent builds must produce identical wasm
  # (and must match the primary build). On a hit: the restored artifact must be
  # byte-identical to a fresh rebuild.
  local result="$1"
  local repro_a="${CARGO_TARGET_DIR}-repro-a"
  local repro_b="${CARGO_TARGET_DIR}-repro-b"
  local repro_a_out repro_b_out
  repro_a_out="$(wasm_out_for "$repro_a")"
  repro_b_out="$(wasm_out_for "$repro_b")"

  rm -rf "$repro_a" "$repro_b"
  mkdir -p "$repro_a" "$repro_b"

  if [[ "$result" == "hit" ]]; then
    # Verify the cached artifact against one fresh independent build.
    run_build "$repro_a"
    compare_wasm_dirs "$WASM_OUT_DIR" "$repro_a_out" "cache-hit vs fresh build"
    summarize "🔬 Reproducibility PASS: cache-hit artifact is byte-identical to a fresh build."
  else
    run_build "$repro_a"
    run_build "$repro_b"
    compare_wasm_dirs "$repro_a_out" "$repro_b_out" "independent build A vs B"
    compare_wasm_dirs "$WASM_OUT_DIR" "$repro_a_out" "primary build vs independent build"
    summarize "🔬 Reproducibility PASS: two independent builds produced byte-identical WASM."
  fi
}

# -----------------------------------------------------------------------------
# Metrics / hit-rate history
# -----------------------------------------------------------------------------
append_history() {
  local result="$1"
  mkdir -p "$(dirname "$HISTORY_FILE")"
  printf '{"key":"%s","result":"%s","ts":%s}\n' \
    "$KEY" "$result" "$(date +%s)" >>"$HISTORY_FILE"
  # Keep only the last 200 entries.
  local tmp="${HISTORY_FILE}.tmp"
  tail -n 200 "$HISTORY_FILE" >"$tmp" && mv "$tmp" "$HISTORY_FILE"
}

compute_hit_rate() {
  TOTAL="$(wc -l <"$HISTORY_FILE" | tr -d '[:space:]')"
  HITS="$(grep -c '"result":"hit"' "$HISTORY_FILE" || true)"
  HITS="${HITS//[[:space:]]/}"
  if [[ "$TOTAL" -gt 0 ]]; then
    HIT_RATE=$((100 * HITS / TOTAL))
  else
    HIT_RATE=0
  fi
}

write_metrics() {
  local result="$1"
  mkdir -p "$(dirname "$METRICS_FILE")"
  printf '{"result":"%s","key":"%s","hash":"%s","hit_rate":%s,"hits":%s,"total":%s}\n' \
    "$result" "$KEY" "$HASH" "$HIT_RATE" "$HITS" "$TOTAL" >"$METRICS_FILE"
}

# -----------------------------------------------------------------------------
# Main
# -----------------------------------------------------------------------------
# Compute the hash in the current shell so HASH/KEY globals survive (a command
# substitution would run the function in a subshell). compute_hash prints KEY.
compute_hash

# Self-test mode: only emit the key so tests can derive the expected cache path.
if [[ "${WASM_CACHE_SELFTEST:-0}" == "1" ]]; then
  exit 0
fi

LOCAL_CACHE_DIR="${LOCAL_CACHE_ROOT}/${KEY}"
echo "check-wasm-cache.sh: key = ${KEY}"

RESULT="miss"
if cache_fetch "$KEY" "$LOCAL_CACHE_DIR"; then
  RESULT="hit"
  copy_cache_to_out "$LOCAL_CACHE_DIR" "$WASM_OUT_DIR"
  summarize "✅ WASM cache HIT for ${KEY} (sources unchanged). Skipping compile (~8 min saved)."
else
  echo "check-wasm-cache.sh: cache miss — building contracts from source"
  run_build "$CARGO_TARGET_DIR"
  cache_push "$KEY" "$WASM_OUT_DIR" "$LOCAL_CACHE_DIR"
  summarize "❌ WASM cache MISS for ${KEY} — compiled from source."
fi

if [[ "$WASM_REPRODUCIBILITY_CHECK" == "1" ]]; then
  reproducibility_check "$RESULT"
fi

append_history "$RESULT"
compute_hit_rate
write_metrics "$RESULT"
summarize "📊 WASM cache hit rate: ${HIT_RATE}% (${HITS} hits / ${TOTAL} builds)"

echo "check-wasm-cache.sh: result=${RESULT} hit_rate=${HIT_RATE}% (${HITS}/${TOTAL})"
exit 0
