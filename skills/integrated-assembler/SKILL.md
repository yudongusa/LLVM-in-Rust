---
name: integrated-assembler
description: Implement issue #141 by formalizing direct MC emission as an explicit integrated assembler stage, with docs/tests/bench coverage.
---

# Integrated Assembler

Use this skill to execute issue #141 with minimal disruption.

## Workflow

1. Add an explicit integrated-assembler API over existing emitter/object serialization.
2. Keep object-format correctness and relocation behavior unchanged.
3. Add tests for assembled bytes/report invariants.
4. Add benchmark hooks comparing codegen paths in `llvm-bench`.
5. Document architecture and migration intent in README/design docs.
6. Run full tests, post PR review, and merge after green checks.

## Minimum validation

```bash
cargo +stable test -p llvm-codegen
cargo +stable test -q
cargo bench -p llvm-bench --bench pipeline -- --warm-up-time 1
```

## Notes

- Avoid introducing text-asm dependencies in default pipeline.
- Prefer additive API (`IntegratedAssembler`) over disruptive rewrites.
