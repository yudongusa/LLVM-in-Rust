#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat >&2 <<'USAGE'
Usage: scripts/sanitizer_matrix.sh <asan-core|tsan-core|miri-core|all>

Runs the targeted sanitizer/UB hardening lanes used by CI. These commands use
nightly Rust because sanitizer support requires unstable flags and Miri.
USAGE
}

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "missing required command: $1" >&2
    exit 1
  fi
}

run_asan_core() {
  require_cmd cargo
  RUSTFLAGS="${RUSTFLAGS:-} -Zsanitizer=address" \
    cargo +nightly test -Zbuild-std --target x86_64-unknown-linux-gnu \
      -p llvm-ir \
      -p llvm-analysis \
      -p llvm-transforms \
      -p llvm-bitcode \
      -p llvm-codegen --lib
}

run_tsan_core() {
  require_cmd cargo
  RUSTFLAGS="${RUSTFLAGS:-} -Zsanitizer=thread" \
    cargo +nightly test -Zbuild-std --target x86_64-unknown-linux-gnu \
      -p llvm-ir \
      -p llvm-analysis \
      -p llvm-transforms \
      -- --test-threads=1
}

run_miri_core() {
  require_cmd cargo
  cargo +nightly miri test \
    -p llvm-ir \
    -p llvm-analysis \
    -p llvm-transforms \
    --lib
}

case "${1:-}" in
  asan-core) run_asan_core ;;
  tsan-core) run_tsan_core ;;
  miri-core) run_miri_core ;;
  all)
    run_asan_core
    run_tsan_core
    run_miri_core
    ;;
  -h|--help) usage ;;
  *) usage; exit 2 ;;
esac
