# Issue #91 Plan

## Acceptance Targets

- Add `llvm-codegen` integration tests for ELF (Linux) and Mach-O (macOS).
- Validate link + run path for simple no-libc style test program where feasible.
- Validate object inspection output via available tools (`readelf`/`nm`/`otool`).
- Fix any discovered object conformance gaps.
- Update README with explicit commands.

## Suggested Order

1. Add test harness with graceful tool discovery.
2. Run tests to capture current failures.
3. Fix serializer/emitter gaps.
4. Add regression assertions.
5. Update docs.
