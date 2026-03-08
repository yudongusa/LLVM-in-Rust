---
name: constant-folding
description: Implement issue #140 by adding a dedicated middle-end constant-folding pass, integrating it into O1+ pipelines, and validating fold/non-fold regressions.
---

# Constant Folding

Use this skill to execute issue #140 with small, correctness-first compiler changes.

## Workflow

1. Implement a dedicated function pass that folds compile-time constant expressions.
2. Reuse existing fold semantics helper (`try_fold`) to avoid divergent rules.
3. Integrate pass into optimization presets (`-O1`, `-O2`, `-O3`).
4. Add regression tests for both folded (`2 + 2`) and non-folded (value-dependent) cases.
5. Run targeted and full test suites.
6. Review PR, post review findings, and merge once checks are green.

## Minimum validation

```bash
cargo +stable test -p llvm-transforms
cargo +stable test -q
```

## Notes

- Preserve behavior on undefined/edge cases (e.g., division by zero stays unfolder).
- Keep pass ordering explicit in `pipeline.rs` to satisfy roadmap traceability.
