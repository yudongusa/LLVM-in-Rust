# Issue #141 Plan

## Acceptance Targets

- Explicit integrated assembler stage/API exists in codebase.
- Default path emits object bytes directly from machine IR without text asm.
- Tests validate the new API and object emission invariants.
- Benchmark hook exists for compile-stage measurement.
- Docs describe architecture and staged rollout.

## Suggested Order

1. Introduce integrated assembler API wrappers/types.
2. Add tests for assembled output/reporting.
3. Wire benchmark usage.
4. Update docs.
5. Validate and merge.
