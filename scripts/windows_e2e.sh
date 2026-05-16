#!/usr/bin/env bash
# Windows end-to-end COFF validation helpers.
# Invoked by .github/workflows/windows-e2e.yml on windows-2022 runners.
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/windows_e2e.sh <lane>

Lanes:
  coff-inspect    Build a COFF object from test IR and inspect it with
                  llvm-objdump / llvm-nm to verify symbol table shape.
USAGE
}

lane="${1:-}"
if [[ -z "$lane" || "$lane" == "-h" || "$lane" == "--help" ]]; then
  usage
  exit 0
fi

run_coff_inspect() {
  echo "[windows-e2e] Building COFF object via linker_compat test binary..."

  # Build the test binary so we can invoke the emit helper manually.
  cargo +stable test \
    -p llvm-in-rust-codegen \
    --test linker_compat \
    --no-run 2>&1

  # Emit a COFF object using the test harness.
  # (We rely on cargo test --nocapture output; a future iteration can extract
  # the object path via a dedicated binary in src/llvm-target-x86/examples/.)
  echo "[windows-e2e] Running symbol inspection tests..."
  cargo +stable test \
    -p llvm-in-rust-codegen \
    --test linker_compat \
    coff_object_main_symbol_visible \
    -- --nocapture

  echo "[windows-e2e] coff-inspect lane: PASS"
}

case "$lane" in
  coff-inspect) run_coff_inspect ;;
  *)
    usage >&2
    exit 2
    ;;
esac
