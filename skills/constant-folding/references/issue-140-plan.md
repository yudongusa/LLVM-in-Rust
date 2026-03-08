# Issue #140 Plan

## Acceptance Targets

- Introduce a first-class constant-folding pass in `llvm-transforms`.
- Ensure O1+ pipelines execute the pass.
- Add tests proving compile-time fold of `2 + 2` and preserving non-foldable cases.
- Keep behavior-preservation guarantees across full test suite.

## Suggested Order

1. Add pass module and export.
2. Wire pass into pipeline presets.
3. Add targeted tests for fold/non-fold semantics.
4. Run targeted/full tests.
5. Open PR, review, and merge.
