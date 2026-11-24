#!/bin/sh
# Deploy every Tessera contract to the sandbox's local Stellar quickstart
# network and write the resulting contract ids to /data/contracts.env.
set -eu

RPC="${SOROBAN_RPC_URL:-http://stellar-node:8000/soroban/rpc}"
PASSPHRASE="${NETWORK_PASSPHRASE:-Standalone Network ; February 2017}"
OUT="${OUT:-/data/contracts.env}"

# The sandbox uses the quickstart root account. It is a local-only key and is
# never used outside this stack.
SOURCE="${SANDBOX_SOURCE_ACCOUNT:-root}"
: > "$OUT"

for contract_dir in /contracts/*/; do
  name=$(basename "$contract_dir")
  [ -d "$contract_dir/src" ] || continue
  manifest="$contract_dir/Cargo.toml"
  [ -f "$manifest" ] || continue

  wasm="/tmp/${name}.wasm"
  echo "building $name" >&2
  if ! (cd /contracts && cargo build --release --target wasm32v1-none -p "$(grep -m1 '^name' "$manifest" | sed 's/.*"\(.*\)".*/\1/')" 2>/dev/null); then
    # Some contracts are library-only crates; skip them rather than failing the
    # whole sandbox boot.
    continue
  fi
  artifact=$(find /contracts/target -name "${name//-/_}.wasm" -o -name "${name}.wasm" | head -n1)
  [ -n "$artifact" ] || continue
  cp "$artifact" "$wasm"

  echo "deploying $name" >&2
  id=$(soroban contract deploy \
    --wasm "$wasm" \
    --source "$SOURCE" \
    --rpc-url "$RPC" \
    --network-passphrase "$PASSPHRASE") || continue
  var=$(printf '%s' "$name" | tr '[:lower:]-' '[:upper:]_')_CONTRACT_ID
  printf '%s=%s\n' "$var" "$id" >> "$OUT"
done

echo "deployed $(wc -l < "$OUT") contracts to $OUT" >&2
