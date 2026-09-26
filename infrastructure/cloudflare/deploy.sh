#!/usr/bin/env bash
#
# Deploy the Tessera edge shield Cloudflare Worker.
#
# Usage:
#   CLOUDFLARE_API_TOKEN=... ./deploy.sh              # create KV (first run) + deploy
#   CLOUDFLARE_API_TOKEN=... ./deploy.sh --dry-run    # bundle only, no account changes
#   CLOUDFLARE_API_TOKEN=... ./deploy.sh --skip-kv    # deploy with an existing KV id
#   ./deploy.sh --help
#
# Environment:
#   CLOUDFLARE_API_TOKEN   required — API token with "Workers Scripts: Edit" and
#                                     "Workers KV Storage: Edit" permissions.
#   CLOUDFLARE_ACCOUNT_ID  optional — required only for multi-account tokens.
#   RATE_LIMIT_KV_ID       optional — skips namespace creation and writes this id.
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

PLACEHOLDER="REPLACE_WITH_KV_NAMESPACE_ID"
DRY_RUN=0
SKIP_KV=0

usage() {
  sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'
}

for arg in "$@"; do
  case "$arg" in
    --dry-run) DRY_RUN=1 ;;
    --skip-kv) SKIP_KV=1 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "error: unknown argument '$arg'" >&2; usage >&2; exit 2 ;;
  esac
done

if ! command -v npx >/dev/null 2>&1; then
  echo "error: npx is required (install Node.js >= 20)." >&2
  exit 1
fi

if [[ -z "${CLOUDFLARE_API_TOKEN:-}" ]]; then
  echo "error: CLOUDFLARE_API_TOKEN is not set." >&2
  echo "       Create a token at https://dash.cloudflare.com/profile/api-tokens" >&2
  exit 1
fi

# ---------------------------------------------------------------------------
# 1. Resolve the RATE_LIMIT KV namespace id.
# ---------------------------------------------------------------------------
if [[ -n "${RATE_LIMIT_KV_ID:-}" && $SKIP_KV -eq 0 ]]; then
  echo "==> using RATE_LIMIT_KV_ID from the environment"
  TMP_FILE="$(mktemp)"
  sed "s/$PLACEHOLDER/$RATE_LIMIT_KV_ID/" wrangler.toml > "$TMP_FILE"
  mv "$TMP_FILE" wrangler.toml
elif [[ $SKIP_KV -eq 0 ]] && grep -q "$PLACEHOLDER" wrangler.toml; then
  echo "==> creating the RATE_LIMIT Workers KV namespace"
  KV_OUTPUT="$(npx --yes wrangler kv namespace create RATE_LIMIT 2>&1)" || {
    echo "$KV_OUTPUT" >&2
    echo "error: 'wrangler kv namespace create RATE_LIMIT' failed." >&2
    exit 1
  }
  KV_ID="$(printf '%s\n' "$KV_OUTPUT" | sed -n 's/.*"id"[[:space:]]*:[[:space:]]*"\([a-f0-9]\{32\}\)".*/\1/p' | head -n1)"
  if [[ -z "$KV_ID" ]]; then
    echo "$KV_OUTPUT" >&2
    echo "error: could not parse the KV namespace id from wrangler output." >&2
    exit 1
  fi
  TMP_FILE="$(mktemp)"
  sed "s/$PLACEHOLDER/$KV_ID/" wrangler.toml > "$TMP_FILE"
  mv "$TMP_FILE" wrangler.toml
  echo "==> wrote KV namespace id $KV_ID into wrangler.toml"
fi

# ---------------------------------------------------------------------------
# 2. Dry run: bundle and validate without touching the account.
# ---------------------------------------------------------------------------
if [[ $DRY_RUN -eq 1 ]]; then
  echo "==> wrangler deploy --dry-run"
  npx --yes wrangler deploy --dry-run --outdir dist
  exit 0
fi

# ---------------------------------------------------------------------------
# 3. Tests, then deploy.
# ---------------------------------------------------------------------------
echo "==> running worker unit tests"
node --test test/*.test.js

echo "==> wrangler deploy"
npx --yes wrangler deploy

echo "==> deployed. Tail logs with: npx wrangler tail"
