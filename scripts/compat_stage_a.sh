#!/usr/bin/env bash
set -euo pipefail

echo "[compat:A] Stage A (core-level subset) starting"

# Core parser + semantic smoke fixtures
cargo +stable test -p llvm-ir-parser --test parse_basic -- --nocapture
cargo +stable test -p llvm-ir-parser --test smoke -- --nocapture

# Basic object/link-compat sanity in the backend
cargo +stable test -p llvm-codegen --test linker_compat tool_presence_report_is_accessible -- --nocapture

echo "[compat:A] Stage A completed successfully"
