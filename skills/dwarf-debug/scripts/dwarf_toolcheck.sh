#!/usr/bin/env bash
set -euo pipefail

for tool in llvm-dwarfdump dwarfdump readelf nm objdump; do
  if command -v "$tool" >/dev/null 2>&1; then
    echo "$tool: found"
  else
    echo "$tool: missing"
  fi
done
