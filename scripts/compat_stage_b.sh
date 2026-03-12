#!/usr/bin/env bash
set -euo pipefail

echo "[compat:B] Stage B (expanded core/alloc proxy suite) starting"

# Differential parser/roundtrip corpus against LLVM oracle.
# Use roundtrip-only subset here to keep this gate focused on compatibility
# breadth without coupling to codegen-hash freeze tests.
cargo +stable test -p llvm-ir-parser --test differential roundtrip_ -- --nocapture

# Keep debug metadata continuity through optimization presets.
cargo +stable test -p llvm-transforms debug_metadata_survives_o1_o2_o3_pipelines -- --nocapture

# Verify debug info sections remain valid in emitted objects.
cargo +stable test -p llvm-codegen --test dwarf_line -- --nocapture

echo "[compat:B] Stage B completed successfully"
