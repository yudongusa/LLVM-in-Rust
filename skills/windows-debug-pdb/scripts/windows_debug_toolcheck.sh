#!/usr/bin/env bash
set -euo pipefail

for t in llvm-readobj llvm-pdbutil lld-link link; do
  if command -v "$t" >/dev/null 2>&1; then
    echo "found: $t"
  else
    echo "missing: $t"
  fi
done
