#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
DIR="$ROOT/tests/alive2/mem2reg"

if ! command -v alive-tv >/dev/null 2>&1; then
  echo "alive-tv not found; install Alive2 to run formal checks"
  exit 0
fi

for before in "$DIR"/*.before.ll; do
  after="${before%.before.ll}.after.ll"
  if [[ ! -f "$after" ]]; then
    echo "missing after file for $before" >&2
    exit 1
  fi
  echo "Verifying $(basename "$before")"
  alive-tv "$before" "$after"
done
