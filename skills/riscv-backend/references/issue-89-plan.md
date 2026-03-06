# Issue #89 Implementation Plan

## Acceptance Checklist

- Add `src/llvm-target-riscv` crate and workspace entry.
- Add register and ABI tests for psABI arg/return mapping.
- Add instruction encoding implementation with at least 30 unit tests.
- Add lowering coverage parity checks versus existing backends.
- Add an end-to-end object generation test for a sample module.

## Execution Order

1. Crate bootstrap and compile skeleton.
2. Register + ABI tests green.
3. Encoder helpers and instruction tests green.
4. Lowering and backend integration.
5. Full test sweep and PR review feedback loop.
