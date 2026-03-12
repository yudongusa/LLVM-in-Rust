#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

ITERATIONS="${ITERATIONS:-10000}"
MAX_TOTAL_TIME="${MAX_TOTAL_TIME:-300}"
CORPUS_DIR="${CORPUS_DIR:-fuzz/corpus/parser}"
LLVM_STRESS_SIZE="${LLVM_STRESS_SIZE:-64}"
LIBFUZZER_TIMEOUT="${LIBFUZZER_TIMEOUT:-60}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --iterations)
      ITERATIONS="$2"
      shift 2
      ;;
    --max-total-time)
      MAX_TOTAL_TIME="$2"
      shift 2
      ;;
    --corpus-dir)
      CORPUS_DIR="$2"
      shift 2
      ;;
    --size)
      LLVM_STRESS_SIZE="$2"
      shift 2
      ;;
    --timeout)
      LIBFUZZER_TIMEOUT="$2"
      shift 2
      ;;
    *)
      echo "unknown arg: $1" >&2
      exit 2
      ;;
  esac
done

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "missing required command: $1" >&2
    exit 1
  fi
}

require_cmd cargo
require_cmd llvm-stress
if ! cargo fuzz --help >/dev/null 2>&1; then
  echo "cargo-fuzz is not installed. Install with: cargo +nightly install cargo-fuzz --locked" >&2
  exit 1
fi

mkdir -p "$CORPUS_DIR"

echo "[fuzz] seeding corpus from llvm-stress (${ITERATIONS} cases, size=${LLVM_STRESS_SIZE})"
for i in $(seq 1 "$ITERATIONS"); do
  llvm-stress -size="$LLVM_STRESS_SIZE" >"$CORPUS_DIR/stress_${i}.ll"
done

echo "[fuzz] running parser target for ${MAX_TOTAL_TIME}s (per-input timeout=${LIBFUZZER_TIMEOUT}s)"
cargo +nightly fuzz run parser "$CORPUS_DIR" -- \
  -max_total_time="$MAX_TOTAL_TIME" \
  -timeout="$LIBFUZZER_TIMEOUT" \
  -print_final_stats=1
