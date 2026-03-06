# Issue 90 Execution Plan

## Goal
Make the workspace usable on stable Rust without losing benchmark coverage.

## Primary Acceptance Criteria

- `cargo +stable build --all-targets` succeeds.
- `cargo +stable test` succeeds.
- Benchmark crate is stable-compatible.
- CI includes stable checks.
- README no longer implies nightly is required for normal development.

## Recommended Order

1. Remove unstable bench harness usage in `src/llvm-bench/benches/`.
2. Add stable benchmarking dependency/config (Criterion preferred).
3. Verify local stable build/test.
4. Update CI workflow with stable job or matrix entry.
5. Update README/docs benchmark + prerequisites text.
6. Re-run stable validation and collect outputs.

## Risk Notes

- Benchmark migration can accidentally change measured scope; preserve fixture and operation boundaries.
- CI changes should avoid doubling runtime excessively; target minimal stable coverage initially.
- Keep migration narrow: avoid unrelated refactors in the same PR.
