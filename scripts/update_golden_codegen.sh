#!/usr/bin/env bash
set -euo pipefail

if [[ "${1:-}" != "--bless" ]]; then
  cat >&2 <<'USAGE'
Usage: scripts/update_golden_codegen.sh --bless

Regenerates the locked golden codegen baseline after an intentional compiler
output change. Commit the resulting baseline diff in the same PR and request
maintainer review/sign-off.
USAGE
  exit 2
fi

export LLVM_IN_RUST_DETERMINISTIC=1
export UPDATE_GOLDEN_CODEGEN=1
export BLESS_GOLDEN_CODEGEN=1
cargo +stable test -p llvm-codegen golden_codegen_objects_match_locked_baseline -- --nocapture
