---
name: mem2reg-verification
description: Implement and verify issue #83 by adding mem2reg correctness invariants, Alive2 before/after corpus, and property-based semantic equivalence tests.
---

# Mem2Reg Verification

Use this skill for issue #83 and similar SSA-promotion verification work.

## Workflow

1. Document formal mem2reg correctness pre/post-conditions in code.
2. Add an Alive2 corpus of before/after `.ll` files under `tests/alive2/mem2reg/`.
3. Add property-based tests that generate random alloca/load/store patterns and check semantic equivalence of original vs mem2reg output by execution.
4. Run targeted and full tests.
5. Post PR review feedback with findings/fixes before merge.

## Minimum validation

```bash
cargo +stable test -p llvm-transforms
cargo +stable test -q
```

## Notes

- Prefer deterministic random generation with bounded IR complexity.
- Keep property tests stable on Linux/macOS (graceful skip if `cc` is unavailable).
