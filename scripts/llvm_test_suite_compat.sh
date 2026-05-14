#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SUITE_DIR="${LLVM_TEST_SUITE_DIR:-$ROOT/target/llvm-test-suite}"
REPORT="${LLVM_TEST_SUITE_REPORT:-$ROOT/target/llvm-test-suite-compat.md}"
LIMIT="${LLVM_TEST_SUITE_LIMIT:-}"
LIMIT_ARGS=()

if [[ -n "$LIMIT" ]]; then
  LIMIT_ARGS=(--limit "$LIMIT")
fi

if [[ ! -d "$SUITE_DIR/.git" ]]; then
  mkdir -p "$(dirname "$SUITE_DIR")"
  git clone --depth 1 https://github.com/llvm/llvm-test-suite.git "$SUITE_DIR"
else
  git -C "$SUITE_DIR" fetch --depth 1 origin main
  git -C "$SUITE_DIR" checkout -q FETCH_HEAD
fi

SCAN_DIR="$SUITE_DIR"
if ! find "$SUITE_DIR" -name '*.ll' -print -quit | grep -q .; then
  if command -v clang >/dev/null 2>&1; then
    GEN_DIR="$ROOT/target/llvm-test-suite-ir"
    rm -rf "$GEN_DIR"
    mkdir -p "$GEN_DIR"
    count=0
    max="${LIMIT:-50}"
    while IFS= read -r src; do
      rel="${src#$SUITE_DIR/}"
      out="$GEN_DIR/${rel%.*}.ll"
      mkdir -p "$(dirname "$out")"
      if clang -S -emit-llvm -O0 -w "$src" -o "$out" >/dev/null 2>&1; then
        count=$((count + 1))
      fi
      if [[ "$count" -ge "$max" ]]; then
        break
      fi
    done < <(find "$SUITE_DIR" \( -name '*.c' -o -name '*.cc' -o -name '*.cpp' \) -type f | sort)
    SCAN_DIR="$GEN_DIR"
    LIMIT_ARGS=()
  else
    echo "warning: no .ll files found and clang is unavailable; report will be empty" >&2
  fi
fi

cmd=(cargo run -p llvm-in-rust --bin llvm-test-suite-compat --locked -- \
  --suite-dir "$SCAN_DIR" \
  --report "$REPORT")
if [[ ${#LIMIT_ARGS[@]} -gt 0 ]]; then
  cmd+=("${LIMIT_ARGS[@]}")
fi
"${cmd[@]}"

cat "$REPORT"
