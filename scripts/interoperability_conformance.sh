#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/interoperability_conformance.sh <lane>

Lanes:
  link        Linker conformance: ld/lld/cc object visibility and link smoke.
  debug       Debugger conformance: DWARF/CodeView line-symbol mapping checks.
  abi         ABI conformance: x86-64, AArch64, and RV64GC calling convention checks.
  mixed       Mixed-toolchain conformance: LLVM parser/roundtrip and external-tool smoke.
  release     Run all release-signoff lanes.
USAGE
}

lane="${1:-}"
if [[ -z "$lane" || "$lane" == "-h" || "$lane" == "--help" ]]; then
  usage
  exit 0
fi

have_tool() { command -v "$1" >/dev/null 2>&1; }

category_group() {
  local category="$1"
  local title="$2"
  echo "::group::[conformance:${category}] ${title}"
}

end_group() { echo "::endgroup::"; }

run_link() {
  category_group link "ld/lld/cc object and linker compatibility"
  for tool in cc ld lld ld.lld llvm-objdump readelf nm otool; do
    if have_tool "$tool"; then
      echo "[conformance:link] found $tool"
    else
      echo "[conformance:link] missing optional tool $tool"
    fi
  done
  cargo +stable test -p llvm-in-rust-codegen --test linker_compat -- --nocapture
  end_group
}

run_debug() {
  category_group debug "debugger symbol and line mapping"
  for tool in gdb lldb llvm-dwarfdump llvm-objdump; do
    if have_tool "$tool"; then
      echo "[conformance:debug] found $tool"
    else
      echo "[conformance:debug] missing optional tool $tool"
    fi
  done
  cargo +stable test -p llvm-in-rust-codegen --test dwarf_line -- --nocapture
  if [[ "$(uname -s)" == MINGW* || "$(uname -s)" == MSYS* || "$(uname -s)" == CYGWIN* ]]; then
    cargo +stable test -p llvm-in-rust-codegen --test codeview_coff -- --nocapture
  else
    cargo +stable test -p llvm-in-rust-codegen --test codeview_coff -- --nocapture
  fi
  end_group
}

run_abi() {
  category_group abi "platform ABI edge cases"
  cargo +stable test -p llvm-in-rust-target-x86 abi -- --nocapture
  cargo +stable test -p llvm-in-rust-target-arm abi -- --nocapture
  cargo +stable test -p llvm-in-rust-target-riscv abi -- --nocapture
  end_group
}

run_mixed() {
  category_group mixed "clang/llc/rustc interop smoke"
  for tool in clang clang-19 llc llc-19 llvm-as llvm-as-19 rustc; do
    if have_tool "$tool"; then
      echo "[conformance:mixed] found $tool"
    else
      echo "[conformance:mixed] missing optional tool $tool"
    fi
  done
  cargo +stable test -p llvm-in-rust-ir-parser --test differential roundtrip_ -- --nocapture
  cargo +stable test -p llvm-in-rust-bitcode --lib -- --nocapture
  end_group
}

case "$lane" in
  link) run_link ;;
  debug) run_debug ;;
  abi) run_abi ;;
  mixed) run_mixed ;;
  release)
    run_link
    run_debug
    run_abi
    run_mixed
    ;;
  *)
    usage >&2
    exit 2
    ;;
esac
