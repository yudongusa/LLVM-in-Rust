# Issue #86 Execution Plan

## Acceptance Criteria Mapping

- [ ] Non-panicking vector lowering at O0 for all vector `InstrKind` variants.
- [ ] SSE4.2 lowering for 12 common patterns with encoding tests.
- [ ] One end-to-end vector program test passes (`.ll -> object -> link -> run`).
- [ ] SIMD emission is feature-gated via `TargetFeatures`.

## Suggested Implementation Order

1. Add `TargetFeatures` in x86 backend API.
2. Add fallback lowering paths for vector instructions (correctness first).
3. Add focused unit tests for vector lowering no-panic coverage.
4. Implement SSE4.2 pattern lowering + encoding tests.
5. Add AVX2 extensions without regressing fallback behavior.
6. Add end-to-end vector smoke test.

## Review Checklist

- Fallback path still works when features disabled.
- No existing scalar integer tests regress.
- SIMD opcodes are encoded with correct prefixes and lane widths.
- Tests cover both enabled and disabled feature modes where applicable.
