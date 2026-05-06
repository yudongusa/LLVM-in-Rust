# Sanitizer and UB hardening gate

This is the Gate 4 release-hardening matrix for sanitizer and undefined-behavior checks.

## Matrix

| Lane | Scope | Trigger | Purpose |
|---|---|---|---|
| ASan core smoke | `llvm-ir`, `llvm-analysis`, `llvm-transforms`, `llvm-bitcode`, `llvm-codegen --lib` | PR, nightly, RC/tag | Catch memory-safety failures in native dependencies and sanitizer-compatible Rust code paths. |
| Miri UB core smoke | `llvm-ir`, `llvm-analysis`, `llvm-transforms --lib` | PR, nightly, RC/tag | Practical UB-hardening lane for the pure Rust core. |
| TSan core lane | `llvm-ir`, `llvm-analysis`, `llvm-transforms` | nightly/manual/RC/tag | Smoke-test thread-safety assumptions; lower signal because the core is mostly single-threaded. |
| Fuzz/differential gates | LLVM-Stress and CSmith workflows | scheduled/manual | Complement sanitizers by exploring parser, optimizer, codegen, and semantic behavior. |

## UBSan feasibility

Rust does not provide a broad stable UBSan equivalent for pure Rust crates. This repository also currently avoids `unsafe` in the core implementation, so the UB gate is intentionally based on:

- Miri for Rust undefined-behavior checks.
- No-`unsafe` audits for the Rust core.
- ASan/TSan smoke lanes for sanitizer-compatible native/toolchain paths.
- CSmith and LLVM-Stress for semantic and parser/codegen stress coverage.

Do not claim a green UBSan result for the Rust core unless a future toolchain provides a meaningful lane.

## Release gate rule

Before cutting an RC or final release tag, the `Sanitizer and UB hardening` workflow must be green for that ref. A new sanitizer, Miri, or UB finding blocks release branches unless a maintainer records an explicit waiver with an issue link and expiry/review condition.

## Suppression policy

Suppressions must be versioned under `ci/sanitizers/` when they are needed. Each suppression requires:

- issue link
- owner
- justification
- affected lane/toolchain
- expiry date or review condition

Unowned or permanent suppressions are not allowed. Prefer fixing the root cause over suppressing.

## Local reproduction

```bash
scripts/sanitizer_matrix.sh asan-core
scripts/sanitizer_matrix.sh miri-core
scripts/sanitizer_matrix.sh tsan-core
scripts/sanitizer_matrix.sh all
```

These commands require nightly Rust. Sanitizer lanes use `-Zbuild-std` and currently target `x86_64-unknown-linux-gnu`.
