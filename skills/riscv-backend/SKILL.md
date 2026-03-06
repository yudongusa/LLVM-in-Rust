---
name: riscv-backend
description: Implement issue #89 by adding an RV64GC backend crate with register definitions, psABI argument/return mapping, instruction selection/lowering parity with existing backends, instruction encoding tests, and ELF emission validation.
---

# RISC-V Backend

Use this skill to execute issue #89 in incremental, test-first steps.

## Workflow

1. Bootstrap `llvm-target-riscv` crate and workspace wiring.
2. Implement register model and ABI helpers.
3. Implement instruction opcodes + encoder with format-focused unit tests.
4. Implement lowering for the same `InstrKind` coverage as x86/aarch64 backends.
5. Validate object emission and end-to-end RISC-V ELF generation.
6. Review PR and post review feedback before merge.

## Step 1: Bootstrap

- Add `src/llvm-target-riscv` crate.
- Add modules: `regs.rs`, `abi.rs`, `instructions.rs`, `lower.rs`, `encode.rs`, `lib.rs`.
- Export `RiscVBackend` and `RiscVEmitter` with interfaces matching existing target crates.

## Step 2: Registers + ABI

- Integer regs: x0..x31 (`x0` hardwired zero).
- ABI mapping: arg regs `a0..a7`, return reg `a0`.
- Keep allocatable and callee-saved sets explicit and tested.

## Step 3: Encoding Core

- Implement helpers for R/I/S/B/U/J type encodings.
- Add at least 30 encoding tests across formats and key opcodes.
- Emit ELF object sections via `llvm_codegen::emit::Emitter`.

## Step 4: Lowering Coverage

- Match lowering coverage expectations used in current backends.
- Unsupported operations must fail deterministically with explicit errors/TODO comments, never panic silently.

## Step 5: Validation

Run at minimum:

```bash
cargo +stable test -p llvm-target-riscv
cargo +stable test
```

If `riscv64-linux-gnu-objdump` is unavailable, document blocker and keep object-shape tests deterministic.

## Step 6: Review PR Feedback

- Review implementation PR for correctness, ABI compliance, and missing tests.
- Post PR review feedback (`gh pr review --comment` or `gh pr comment`).
- Fix findings in follow-up commits before merge.

## Resources

- Use [`references/issue-89-plan.md`](references/issue-89-plan.md) for acceptance checklist.
- Use [`scripts/riscv_coverage_audit.sh`](scripts/riscv_coverage_audit.sh) to track lowering/encoding progress.
