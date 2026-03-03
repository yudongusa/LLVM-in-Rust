# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

LLVM-in-Rust is a **pure Rust re-implementation of LLVM** — no C++ or FFI dependencies. The goal is to support the full compiler pipeline: LLVM IR → optimization passes → machine code generation.

## Common Commands

```bash
cargo build                   # Build all crates
cargo build --release         # Release build
cargo test                    # Run all tests
cargo test <test_name>        # Run a single test
cargo test -p <crate>         # Test a specific crate
cargo clippy                  # Lint
cargo fmt                     # Format
cargo check                   # Type-check without building
```

## Architecture

The project is a Cargo workspace. Each major compiler stage is its own crate:

```
llvm-ir/          # Core IR types (types, values, instructions, modules)
llvm-analysis/    # Analysis passes (dominator tree, CFG, use-def chains)
llvm-transforms/  # Optimization passes (DCE, mem2reg, constant folding, inlining)
llvm-codegen/     # Target-independent code generation (instruction selection, regalloc)
llvm-target-x86/  # x86_64 target backend
llvm-target-arm/  # AArch64 target backend
llvm-bitcode/     # LLVM bitcode binary format (.bc)
llvm-ir-parser/   # LLVM IR text format parser (.ll)
llvm/             # Top-level crate tying everything together
```

## Design

### IR Layer (`llvm-ir`)
The IR is the foundation everything else builds on. Key types:
- **`Module`** — top-level container (globals, functions, metadata)
- **`Function`** — list of `BasicBlock`s, signature, attributes
- **`BasicBlock`** — ordered list of `Instruction`s ending in a terminator
- **`Instruction`** — SSA instruction (arithmetic, memory, control flow, calls)
- **`Type`** — integer, float, pointer, vector, struct, array, function types
- **`Value`** — anything that produces a result: constants, instructions, arguments

All values are in **SSA form**. Use an arena allocator (e.g., `typed-arena` or `bumpalo`) to manage lifetimes within a module.

### Analysis Layer (`llvm-analysis`)
Analyses are computed on demand and cached. Key analyses:
- **Dominator tree** — required by most optimizations
- **Control flow graph (CFG)** — predecessor/successor maps over basic blocks
- **Use-def / def-use chains** — track where each SSA value is defined and used
- **Loop info** — identify natural loops (built on top of dominator tree)

### Optimization Passes (`llvm-transforms`)
Passes implement a common `Pass` trait. A **PassManager** sequences and pipelines them.

Planned passes (in implementation order):
1. `mem2reg` — promote `alloca`/`load`/`store` to SSA values (requires dominator tree)
2. Constant folding / constant propagation
3. Dead code elimination (DCE)
4. Common subexpression elimination (CSE)
5. Function inlining
6. Loop-invariant code motion (LICM)
7. Loop unrolling

### Code Generation (`llvm-codegen` + target crates)
Code generation is split into target-independent and target-specific stages:

1. **Legalization** — lower IR types/ops not natively supported by the target
2. **Instruction selection** — map IR instructions to target machine instructions using DAG pattern matching
3. **Register allocation** — assign virtual registers to physical registers (linear scan initially, graph coloring later)
4. **Instruction scheduling** — reorder instructions to improve ILP
5. **Machine code emission** — serialize to ELF (Linux), Mach-O (macOS), or COFF (Windows)

Target backends (`llvm-target-x86`, `llvm-target-arm`) provide:
- Register definitions and register classes
- Instruction definitions with encoding and operand constraints
- Calling convention descriptions
- Platform-specific ABI handling

### Bitcode & IR Text Format
- `llvm-bitcode` — read/write the LLVM bitcode binary format (`.bc`) for interop with LLVM tools
- `llvm-ir-parser` — parse the LLVM IR text format (`.ll`) for testing and debugging

## Implementation Phases

### Phase 1 — IR Foundation
- [ ] Define core IR types (`Type`, `Value`, `Instruction`, `BasicBlock`, `Function`, `Module`)
- [ ] Implement IR builder API (programmatic IR construction)
- [ ] IR printer (emit `.ll` text format)
- [ ] Basic IR parser (read `.ll` files)
- [ ] Unit tests for IR construction and round-trip printing

### Phase 2 — Analysis Infrastructure
- [ ] CFG construction and traversal
- [ ] Dominator tree (Lengauer-Tarjan algorithm)
- [ ] Use-def and def-use chains
- [ ] Loop detection

### Phase 3 — Optimization Passes
- [ ] Pass trait and PassManager
- [ ] `mem2reg`
- [ ] Constant folding and propagation
- [ ] DCE
- [ ] Inlining

### Phase 4 — x86_64 Backend
- [ ] Target description (registers, instructions)
- [ ] Instruction selection (DAG lowering)
- [ ] Register allocator (linear scan)
- [ ] ELF and Mach-O object file emission

### Phase 5 — AArch64 Backend & Bitcode
- [ ] AArch64 target
- [ ] Bitcode reader/writer (`.bc` format)

## Key Design Decisions

- **Pure Rust, no LLVM C API** — the entire pipeline is implemented in Rust with no FFI
- **Arena allocation** for IR nodes to avoid reference cycles and enable fast allocation
- **SSA form throughout** — IR is always in SSA; `mem2reg` is run early to eliminate non-SSA patterns from frontends
- **Trait-based pass system** — analyses and transforms implement common traits so the pass manager can pipeline them generically
- **Target descriptions are data-driven** where possible (register classes, instruction patterns) to keep target backends concise
