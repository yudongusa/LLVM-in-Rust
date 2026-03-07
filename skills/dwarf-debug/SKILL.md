---
name: dwarf-debug
description: Implement issue #92 by threading LLVM `!dbg` metadata and emitting DWARF debug sections (starting with `.debug_line`) with validation via toolchain utilities.
---

# DWARF Debug

Use this skill to execute issue #92 with incremental, test-backed DWARF support.

## Workflow

1. Parse and preserve `!dbg` / `!DILocation` metadata in IR.
2. Thread debug locations into codegen structures.
3. Emit `.debug_line` for ELF objects when debug metadata is present.
4. Validate with integration tests + external tools when available (`readelf`, `llvm-dwarfdump`, `dwarfdump`).
5. Update docs and PR with explicit limitations/follow-ups.
6. Review PR, run full tests, post review feedback before merge.

## Minimum validation

```bash
cargo +stable test -p llvm-ir-parser
cargo +stable test -p llvm-codegen
cargo +stable test -q
```

## Notes

- Keep output deterministic and avoid host-only assumptions in tests.
- If full DWARF verification is not yet complete, keep scope explicit and open follow-up issues.
