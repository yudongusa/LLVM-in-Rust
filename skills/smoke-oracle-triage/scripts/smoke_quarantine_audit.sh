#!/usr/bin/env bash
set -euo pipefail

rg -n "smoke_(fibonacci_iterative|collatz_steps|max_select|nested_loop)|#\[ignore" src/llvm-ir-parser/tests/smoke.rs
