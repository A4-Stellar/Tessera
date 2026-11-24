#!/usr/bin/env bash
# Build the token-economics simulator to WebAssembly for the docs site.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
TARGET_DIR="$ROOT/target/wasm32-unknown-unknown/release"
OUT_DIR="$(cd "$ROOT/../.." && pwd)/public/wasm"
OUT="$OUT_DIR/token_economics.wasm"

rustup target add wasm32-unknown-unknown >/dev/null 2>&1 || true
cargo build --manifest-path "$ROOT/Cargo.toml" --release --target wasm32-unknown-unknown
mkdir -p "$OUT_DIR"
cp "$TARGET_DIR/tessera_token_economics.wasm" "$OUT"
echo "wrote $OUT ($(wc -c < "$OUT") bytes)"
