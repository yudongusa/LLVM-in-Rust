#!/usr/bin/env bash
set -euo pipefail
rg -n "ConstantFold|ConstProp|build_pipeline\(OptLevel::O[123]\)" src/llvm-transforms/src
