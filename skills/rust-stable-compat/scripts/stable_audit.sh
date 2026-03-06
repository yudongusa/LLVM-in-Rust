#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
cd "$repo_root"

echo "[stable-audit] repo: $repo_root"
echo "[stable-audit] rustup active:"
rustup show active-toolchain || true

echo
echo "[stable-audit] nightly feature scan"
if command -v rg >/dev/null 2>&1; then
  rg -n "#!\[feature\(" src examples || true
else
  grep -RIn "#!\[feature(" src examples || true
fi

echo
echo "[stable-audit] benchmark harness scan"
if command -v rg >/dev/null 2>&1; then
  rg -n "extern crate test|\#\[bench\]|criterion" src/llvm-bench || true
else
  grep -RIn "extern crate test\|\#\[bench\]\|criterion" src/llvm-bench || true
fi

echo
echo "[stable-audit] done"
