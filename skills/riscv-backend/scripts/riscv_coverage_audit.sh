#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/../../.." && pwd)"
cd "$repo_root"

echo "== RISC-V crate presence =="
rg -n "llvm-target-riscv" Cargo.toml src/*/Cargo.toml || true

echo
echo "== Lowering TODO/FIXME markers =="
rg -n "TODO|FIXME|unimplemented!\(|panic!\(" src/llvm-target-riscv || true

echo
echo "== Encoding test count =="
rg -n "^\s*#\[test\]" src/llvm-target-riscv/src/encode.rs | wc -l | awk '{print $1 " tests"}'
