#!/usr/bin/env bash
# =============================================================================
# optimize_wasm.sh — build, optimise and size-gate every Tessera Soroban WASM
# =============================================================================
#
# USAGE
#   bash contracts/scripts/optimize_wasm.sh [--help]
#
# Pipeline (all paths resolved from this script, so any cwd works):
#   1. cargo build --release --target wasm32-unknown-unknown
#      (uses the existing [profile.release] in contracts/Cargo.toml:
#       opt-level="z", lto, codegen-units=1, panic="abort", strip="symbols")
#   2. wasm-opt -Oz --strip-debug on every produced *.wasm, in place.
#   3. python3 check_wasm_size.py --wasm-dir <release> --budget size_budget.json
#      (the authoritative size gate; exit status is propagated).
#
# Every command actually executed is echoed with a leading '+' so CI logs
# contain the exact reproduction commands.
#
# Environment variables (defaults in parentheses):
#   CONTRACTS_DIR   (<repo>/contracts)                       Cargo workspace root
#   WASM_DIR        (<CONTRACTS_DIR>/target/wasm32-unknown-unknown/release)
#   BUDGET          (<CONTRACTS_DIR>/size_budget.json)        Size budget JSON
#   SIZE_REPORT     (empty)                                  Optional --json report path
#   CARGO           (cargo)                                  Cargo binary
#   CARGO_TARGET_DIR (unset = Cargo default)                  Override target dir (WASM_DIR must match)
#   WASM_OPT_FLAGS  (-Oz --strip-debug)                       Flags passed to wasm-opt
#   SKIP_WASM_OPT   (0)                                       Set 1 to continue without wasm-opt
#
# If wasm-opt is absent the script prints a clear error and exits 1, unless
# SKIP_WASM_OPT=1 is set, in which case it warns and size-checks the raw WASM.
#
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: bash contracts/scripts/optimize_wasm.sh [--help]

Build, optimise (wasm-opt -Oz --strip-debug) and size-check all Soroban
contract WASM artifacts. See the script header for environment variables.
EOF
}

for arg in "$@"; do
  case "$arg" in
    --help | -h)
      usage
      exit 0
      ;;
    *)
      echo "optimize_wasm.sh: unknown argument: $arg" >&2
      usage >&2
      exit 2
      ;;
  esac
done

# -----------------------------------------------------------------------------
# Resolve paths from the script location (works from any cwd).
# -----------------------------------------------------------------------------
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/../.." && pwd)"

CONTRACTS_DIR="${CONTRACTS_DIR:-${REPO_ROOT}/contracts}"
WASM_DIR="${WASM_DIR:-${CONTRACTS_DIR}/target/wasm32-unknown-unknown/release}"
BUDGET="${BUDGET:-${CONTRACTS_DIR}/size_budget.json}"
CHECKER="${CHECKER:-${SCRIPT_DIR}/check_wasm_size.py}"
SIZE_REPORT="${SIZE_REPORT:-}"
CARGO="${CARGO:-cargo}"
WASM_OPT_FLAGS="${WASM_OPT_FLAGS:--Oz --strip-debug}"
SKIP_WASM_OPT="${SKIP_WASM_OPT:-0}"

if [[ ! -f "${CONTRACTS_DIR}/Cargo.toml" ]]; then
  echo "optimize_wasm.sh: not a Cargo workspace: ${CONTRACTS_DIR}/Cargo.toml" >&2
  exit 1
fi
if [[ ! -f "${BUDGET}" ]]; then
  echo "optimize_wasm.sh: size budget not found: ${BUDGET}" >&2
  exit 1
fi
if [[ ! -f "${CHECKER}" ]]; then
  echo "optimize_wasm.sh: size checker not found: ${CHECKER}" >&2
  exit 1
fi

echo "optimize_wasm.sh: repo root   = ${REPO_ROOT}"
echo "optimize_wasm.sh: contracts  = ${CONTRACTS_DIR}"
echo "optimize_wasm.sh: wasm dir   = ${WASM_DIR}"
echo "optimize_wasm.sh: budget     = ${BUDGET}"

# -----------------------------------------------------------------------------
# 1. Build all workspace contracts for wasm32-unknown-unknown / release.
#    The release profile in contracts/Cargo.toml is intentionally left as-is.
# -----------------------------------------------------------------------------
build_cmd=("${CARGO}" build --release --target wasm32-unknown-unknown --manifest-path "${CONTRACTS_DIR}/Cargo.toml")
# Run from the workspace root so rustup honours contracts/rust-toolchain.toml
# (the same pinned toolchain the existing CI wasm-build job uses).
echo "+ (cd ${CONTRACTS_DIR} && ${build_cmd[*]})"
(cd "${CONTRACTS_DIR}" && "${build_cmd[@]}")

# -----------------------------------------------------------------------------
# 2. Optimise in place. Fail loudly if wasm-opt is missing unless explicitly
#    asked to skip, so CI can never silently publish unoptimised artifacts.
# -----------------------------------------------------------------------------
shopt -s nullglob
wasm_files=("${WASM_DIR}"/*.wasm)
shopt -u nullglob

if (( ${#wasm_files[@]} == 0 )); then
  echo "optimize_wasm.sh: build produced no .wasm files in ${WASM_DIR}" >&2
  exit 1
fi

TMP_WASM=""
cleanup() {
  if [[ -n "${TMP_WASM}" && -f "${TMP_WASM}" ]]; then
    rm -f "${TMP_WASM}"
  fi
}
trap cleanup EXIT

if command -v wasm-opt >/dev/null 2>&1; then
  echo "optimize_wasm.sh: wasm-opt found: $(command -v wasm-opt)"
  # shellcheck disable=SC2206  # WASM_OPT_FLAGS is deliberately word-split.
  opt_flags=(wasm-opt ${WASM_OPT_FLAGS})
  for wasm in "${wasm_files[@]}"; do
    before="$(wc -c <"${wasm}" | tr -d '[:space:]')"
    TMP_WASM="$(mktemp "${wasm}.opt.XXXXXX")"
    opt_cmd=("${opt_flags[@]}" "${wasm}" -o "${TMP_WASM}")
    echo "+ ${opt_cmd[*]}"
    "${opt_cmd[@]}"
    mv -f "${TMP_WASM}" "${wasm}"
    TMP_WASM=""
    after="$(wc -c <"${wasm}" | tr -d '[:space:]')"
    echo "optimize_wasm.sh: $(basename "${wasm}"): ${before} -> ${after} bytes"
  done
else
  echo "optimize_wasm.sh: ERROR: wasm-opt was not found in PATH." >&2
  if [[ "${SKIP_WASM_OPT}" == "1" ]]; then
    echo "optimize_wasm.sh: SKIP_WASM_OPT=1 is set: skipping optimisation; raw WASM will be size-checked." >&2
  else
    echo "optimize_wasm.sh: install binaryen (e.g. 'apt-get install -y binaryen') or set SKIP_WASM_OPT=1 to continue without optimisation." >&2
    exit 1
  fi
fi

# -----------------------------------------------------------------------------
# 3. Size gate. This is the authoritative check and its exit code is propagated.
# -----------------------------------------------------------------------------
check_cmd=(python3 "${CHECKER}" --wasm-dir "${WASM_DIR}" --budget "${BUDGET}")
if [[ -n "${SIZE_REPORT}" ]]; then
  check_cmd+=(--json "${SIZE_REPORT}")
fi
echo "+ ${check_cmd[*]}"
"${check_cmd[@]}"

echo "optimize_wasm.sh: pipeline complete."
