# Golden codegen gate

Issue #178 locks a small representative corpus to deterministic x86_64 ELF
object output. The gate catches unapproved codegen drift before it reaches
`main`.

## What is covered

The corpus in `src/llvm-codegen/tests/golden_codegen.rs` includes parser,
optimization, backend/control-flow, and debug/unwind-oriented cases. Each case
is lowered with `LLVM_IN_RUST_DETERMINISTIC=1`, emitted twice in-process, and
hashed as object bytes in `src/llvm-codegen/tests/golden/codegen_x86_64_elf.json`.

## Normal CI path

CI runs:

```bash
LLVM_IN_RUST_DETERMINISTIC=1 cargo +stable test -p llvm-in-rust-codegen golden -- --nocapture
```

A PR fails if any emitted object checksum differs from the committed baseline.
Re-runs on the same commit must produce identical bytes.

## Blessed update path

Only use this for intentional output changes:

```bash
scripts/update_golden_codegen.sh --bless
```

Then commit the baseline diff with the compiler change. The PR must explain the
reason for the drift and receive maintainer approval. CI also rejects baseline
file changes unless the PR is labeled `approved-golden-update`, which maintainers
use as the explicit sign-off.

## Triage unexpected drift

1. Re-run the golden test locally on a clean tree.
2. If only one case changed, inspect the responsible lowering/pass change.
3. If all cases changed, inspect object serialization, section ordering, symbol
   ordering, timestamps, seeds, and pass iteration order first.
4. If the drift is correct and intentional, regenerate with the blessed path and
   document why in the PR.
