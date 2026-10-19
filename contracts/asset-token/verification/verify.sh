#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"
command -v kompile >/dev/null
command -v kprove >/dev/null
kompile mint_burn.k --backend haskell --main-module MINT-BURN \
  --syntax-module MINT-BURN-SYNTAX --output-definition .kbuild
kprove mint_burn-spec.k --definition .kbuild --spec-module MINT-BURN-SPEC "$@"
