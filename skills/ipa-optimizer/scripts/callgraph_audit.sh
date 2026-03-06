#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"

echo "[callgraph-audit] call-like IR instructions"
rg -n "InstrKind::Call|Call \{" \
  "$ROOT/src/llvm-ir/src/instruction.rs" \
  "$ROOT/src/llvm-ir-parser" \
  "$ROOT/src/llvm-transforms" || true

echo "[callgraph-audit] pipeline integration points"
rg -n "OptLevel::O3|add_module_pass|Inliner|Gvn|LoopUnroll" \
  "$ROOT/src/llvm-transforms/src/pipeline.rs" || true
