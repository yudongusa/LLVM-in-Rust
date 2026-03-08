#!/usr/bin/env bash
set -euo pipefail
rg -n "IntegratedAssembler|assemble_with_report|emit_object\(" src/llvm-codegen src/llvm-bench
