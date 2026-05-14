# Codegen quality gate

This directory contains the canonical LLVM IR kernels used by the issue #208 output code-quality gate.

The gate records, per kernel:

- emitted x86_64 `.text` instruction count,
- backend spill/reload instruction count,
- `.text` byte size,
- prologue/epilogue byte size,
- LLVM `llc -O2` comparison metrics for the same IR.

Run the gate locally with:

```sh
python3 scripts/codegen_quality_gate.py
```

Locked metrics live in `bench/codegen/baselines.json`. Updating them is intentionally explicit and requires a sign-off comment:

```sh
CODEGEN_QUALITY_BASELINE_SIGNOFF="why this drift is intentional" make update-baselines
```

CI fails when any locked metric regresses by more than 10%.
