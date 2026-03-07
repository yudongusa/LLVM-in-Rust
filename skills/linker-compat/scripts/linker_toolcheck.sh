#!/usr/bin/env bash
set -euo pipefail

for t in cc ld lld readelf nm objdump otool; do
  if command -v "$t" >/dev/null 2>&1; then
    echo "$t: OK ($(command -v "$t"))"
  else
    echo "$t: MISSING"
  fi
done
