#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

COUNT="${COUNT:-1000}"
WORK_DIR="${WORK_DIR:-$(mktemp -d)}"
CSMITH_INCLUDE="${CSMITH_INCLUDE:-/usr/include/csmith}"
KEEP_FAILURES_DIR="${KEEP_FAILURES_DIR:-$repo_root/csmith_failures}"
CC_BIN="${CC_BIN:-clang}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --count)
      COUNT="$2"
      shift 2
      ;;
    --work-dir)
      WORK_DIR="$2"
      shift 2
      ;;
    --csmith-include)
      CSMITH_INCLUDE="$2"
      shift 2
      ;;
    --cc)
      CC_BIN="$2"
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

run_with_timeout() {
  if command -v timeout >/dev/null 2>&1; then
    timeout "$1" "$2" "${@:3}"
  elif command -v gtimeout >/dev/null 2>&1; then
    gtimeout "$1" "$2" "${@:3}"
  else
    "$2" "${@:3}"
  fi
}

if [[ "$(uname -s)" != "Linux" ]]; then
  echo "csmith_test.sh currently supports Linux runners only." >&2
  exit 2
fi
require_cmd csmith
require_cmd "$CC_BIN"
require_cmd cargo
mkdir -p "$WORK_DIR" "$KEEP_FAILURES_DIR"

mismatch=0
compile_fail=0
runtime_fail=0

for i in $(seq 1 "$COUNT"); do
  c_file="$WORK_DIR/prog_${i}.c"
  ll_file="$WORK_DIR/prog_${i}.ll"
  clang_bin="$WORK_DIR/prog_${i}_clang"

  csmith >"$c_file"

  if ! "$CC_BIN" -O0 -w -I"$CSMITH_INCLUDE" "$c_file" -o "$clang_bin"; then
    compile_fail=$((compile_fail + 1))
    continue
  fi

  if ! "$CC_BIN" -S -emit-llvm -O0 -Xclang -disable-O0-optnone -w -I"$CSMITH_INCLUDE" "$c_file" -o "$ll_file"; then
    compile_fail=$((compile_fail + 1))
    continue
  fi

  set +e
  run_with_timeout 5s "$clang_bin" >/dev/null 2>&1
  clang_exit=$?
  set -e

  if [[ "$clang_exit" -eq 124 || "$clang_exit" -eq 137 ]]; then
    runtime_fail=$((runtime_fail + 1))
    continue
  fi

  set +e
  ours_exit="$(cargo run -q -p llvm-ir-parser --example run_ir -- "$ll_file" 2>/dev/null)"
  ours_status=$?
  set -e

  if [[ "$ours_status" -ne 0 ]]; then
    runtime_fail=$((runtime_fail + 1))
    cp "$c_file" "$KEEP_FAILURES_DIR/failure_${i}.c"
    cp "$ll_file" "$KEEP_FAILURES_DIR/failure_${i}.ll"
    continue
  fi

  if [[ "$ours_exit" != "$clang_exit" ]]; then
    mismatch=$((mismatch + 1))
    cp "$c_file" "$KEEP_FAILURES_DIR/mismatch_${i}.c"
    cp "$ll_file" "$KEEP_FAILURES_DIR/mismatch_${i}.ll"
  fi

done

echo "[csmith] total=$COUNT compile_fail=$compile_fail runtime_fail=$runtime_fail mismatch=$mismatch"

if [[ "$mismatch" -ne 0 || "$runtime_fail" -ne 0 ]]; then
  exit 1
fi
