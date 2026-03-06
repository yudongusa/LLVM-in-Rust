#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"

echo "[vector-audit] vector instruction lowering sites"
rg -n "ExtractElement|InsertElement|ShuffleVector|Vector|TODO|not yet supported" \
  "$ROOT/src/llvm-target-x86/src/lower.rs" \
  "$ROOT/src/llvm-target-x86/src/encode.rs" || true

echo "[vector-audit] vector IR parser/printer coverage"
rg -n "extractelement|insertelement|shufflevector" \
  "$ROOT/src/llvm-ir-parser" \
  "$ROOT/src/llvm-ir/src" || true
