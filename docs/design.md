# LLVM-in-Rust: Design Document

## Overview

LLVM-in-Rust is a pure-Rust reimplementation of the LLVM compiler infrastructure.
It covers the full compiler pipeline — LLVM IR parsing, analysis, optimization, and
machine-code generation for x86_64 and AArch64 — with no C++ FFI or dependency on
the upstream LLVM C API.

---

## Goals and Non-Goals

### Goals
- Faithful reimplementation of LLVM IR semantics in safe Rust
- Full SSA form throughout: every value is defined exactly once
- Target-independent analysis and optimization passes
- Native code generation for x86_64 (System V ABI) and AArch64 (AAPCS64)
- Round-trip serialization: IR → binary → IR via the LRIR format
- Zero unsafe code except where unavoidable at system boundaries

### Non-Goals
- Binary compatibility with LLVM's C++ API
- Full LLVM bitcode (`.bc`) compatibility (we use the simpler LRIR format)
- JIT compilation
- Debug-information (DWARF) emission in the initial phases

---

## Repository Structure

```
llvm-ir/           Core IR type system, builder, printer
llvm-ir-parser/    Text format (.ll) parser
llvm-analysis/     CFG, dominator tree, use-def, loop detection
llvm-transforms/   Optimization passes (mem2reg, DCE, constant folding, inlining)
llvm-codegen/      Target-independent machine IR, register allocator, object emitter
llvm-target-x86/   x86_64 instruction selection, encoding, ELF/Mach-O emission
llvm-target-arm/   AArch64 instruction selection, encoding, ELF/Mach-O emission
llvm-bitcode/      LRIR binary serialization (reader + writer)
llvm/              Top-level crate re-exporting all public APIs
docs/              This document and the user manual
```

Each directory is an independent Cargo crate. The workspace root `Cargo.toml` lists
all members. Crate dependencies follow the pipeline order:

```
llvm-ir ← llvm-ir-parser
llvm-ir ← llvm-analysis ← llvm-transforms
llvm-ir, llvm-analysis ← llvm-codegen ← llvm-target-x86
                                       ← llvm-target-arm
llvm-ir ← llvm-bitcode
all of the above ← llvm
```

---

## Crate-by-Crate Design

### 1. `llvm-ir` — Core IR

The IR layer is the foundation everything else builds on.

#### Memory Model

All IR objects use **Vec-backed arenas** (no `unsafe`, no external arena crate):

| Container | Arena | Index type |
|-----------|-------|------------|
| `Context` | `types: Vec<TypeData>` | `TypeId(u32)` |
| `Context` | `constants: Vec<ConstantData>` | `ConstId(u32)` |
| `Function` | `instructions: Vec<Instruction>` | `InstrId(u32)` |
| `Module` | `functions: Vec<Function>` | `FunctionId(u32)` |
| `Module` | `globals: Vec<GlobalVariable>` | `GlobalId(u32)` |
| `Function` | `blocks: Vec<BasicBlock>` | `BlockId(u32)` |
| `Function` | `args: Vec<Argument>` | `ArgId(u32)` |

All index types are `Copy` newtypes over `u32`.

#### `ValueRef` — Universal Operand

```rust
pub enum ValueRef {
    Instruction(InstrId),   // result of an SSA instruction
    Argument(ArgId),        // function parameter
    Constant(ConstId),      // interned constant
    Global(GlobalId),       // global variable or function reference
}
```

`ValueRef` is `Copy` and used as every instruction operand, phi incoming, and
call argument. It replaces raw pointers with arena-safe indices.

#### `Context`

`Context` is the shared type and constant pool. It:
- Interns structural types (`TypeData`) via a `HashMap<TypeData, TypeId>` dedup map
- Stores nominal (named) struct types separately in `named_struct_map` to support
  recursive types without infinite hash loops
- Pre-interns common singletons: `void_ty`, `i1_ty`, `i8_ty`, `i16_ty`, `i32_ty`,
  `i64_ty`, `f32_ty`, `f64_ty`, `ptr_ty`

#### `TypeData`

```rust
pub enum TypeData {
    Void,
    Integer(u32),                          // bit width
    Float(FloatKind),                      // Half, BFloat, Single, Double, …
    Pointer,                               // opaque pointer (LLVM 15+ style)
    Array  { element: TypeId, len: u64 },
    Vector { element: TypeId, len: u32, scalable: bool },
    Struct(StructType),                    // { name, fields, packed }
    Function(FunctionType),                // { ret, params, variadic }
    Label,
    Metadata,
}
```

#### `InstrKind`

All 94+ LLVM IR instruction variants are represented:

- **Integer arithmetic**: `Add`, `Sub`, `Mul`, `UDiv`, `SDiv`, `URem`, `SRem`
  (each with `nuw`/`nsw`/`exact` flags)
- **Bitwise**: `And`, `Or`, `Xor`, `Shl`, `LShr`, `AShr`
- **FP arithmetic**: `FAdd`, `FSub`, `FMul`, `FDiv`, `FRem`, `FNeg`
  (with `FastMathFlags`)
- **Comparisons**: `ICmp` (`IntPredicate`), `FCmp` (`FloatPredicate`)
- **Memory**: `Alloca`, `Load`, `Store`, `GetElementPtr`
- **Casts**: `Trunc`, `ZExt`, `SExt`, `FPTrunc`, `FPExt`, `FPToUI`, `FPToSI`,
  `UIToFP`, `SIToFP`, `PtrToInt`, `IntToPtr`, `BitCast`, `AddrSpaceCast`
- **Misc**: `Select`, `Phi`, `ExtractValue`, `InsertValue`,
  `ExtractElement`, `InsertElement`, `ShuffleVector`
- **Calls**: `Call { callee, callee_ty, args, tail: TailCallKind }`
- **Terminators**: `Ret`, `Br`, `CondBr`, `Switch`, `Unreachable`

#### `Builder`

`Builder<'a>` holds mutable references to both a `Context` and a `Module` and
provides ergonomic methods for constructing IR:

```rust
let mut b = Builder::new(&mut ctx, &mut module);
b.add_function("add", i64_ty, vec![i64_ty, i64_ty], vec!["a","b"], false, Linkage::External);
let entry = b.add_block("entry");
b.position_at_end(entry);
let sum = b.build_add("sum", b.get_arg(0), b.get_arg(1));
b.build_ret(sum);
```

The builder auto-numbers unnamed values (`""` → `"1"`, `"2"`, …) and registers
every new `InstrId` in the function's `value_names` map.

#### `Printer`

`Printer<'a>` produces standard LLVM `.ll` text output from a `(Context, Module)`
pair. It emits module headers, named struct definitions, globals, and function
bodies with correct type annotations on every operand.

---

### 2. `llvm-ir-parser` — Text Parser

A hand-rolled recursive-descent parser reading LLVM IR text (`.ll`) files.

- `Lexer`: hand-rolled scanner with 1-token lookahead. Handles `iNN` integer
  types dynamically, hex float literals (`0xHHH...`), all LLVM keywords.
- `Parser`: entry point `parse(src: &str) -> Result<(Context, Module), ParseError>`.
  Forward block references via `pending_blocks: HashMap<String, BlockId>` —
  allocated on first `br` reference and filled when the label is encountered.
  Named struct forward references use the same two-phase pattern.

---

### 3. `llvm-analysis` — Analysis Passes

All analyses are pure functions over immutable IR; results are returned by value
and cached by callers.

| Type | Description |
|------|-------------|
| `Cfg` | Predecessor/successor maps over `BlockId`s |
| `DomTree` | Lengauer-Tarjan immediate-dominator algorithm; `dominates(a, b)` query |
| `LoopInfo` | Natural loop detection (back edges via DomTree); nesting depth |
| `UseDefInfo` | SSA use-def chains: for each `InstrId`, the set of instructions that use its result |

---

### 4. `llvm-transforms` — Optimization Passes

Passes implement one of two traits:

```rust
pub trait FunctionPass { fn run(&mut self, ctx: &mut Context, func: &mut Function); }
pub trait ModulePass   { fn run(&mut self, ctx: &mut Context, module: &mut Module); }
```

`PassManager` sequences passes and pipelines them:

| Pass | Trait | Description |
|------|-------|-------------|
| `Mem2Reg` | FunctionPass | Promote `alloca`/`load`/`store` to SSA φ-nodes (Cytron algorithm: IDF + rename DFS) |
| `ConstantFold` (`try_fold`) | — | Fold a single instruction to a constant, if possible |
| `ConstProp` | FunctionPass | Sparse constant propagation in RPO order |
| `DeadCodeElim` | FunctionPass | Remove instructions with no uses and no side effects |
| `Inliner` | ModulePass | Inline direct calls; clones callee with block/instr offset remapping |

---

### 5. `llvm-codegen` — Target-Independent Code Generation

Provides the machine IR data model and the register allocator. Target backends
implement `IselBackend` and `Emitter`.

#### Machine IR

```
MachineFunction
  └─ Vec<MachineBlock>
       └─ Vec<MInstr>
            ├─ opcode: MOpcode      (target-specific constant)
            ├─ dst:    Option<VReg> (result virtual register)
            ├─ operands: Vec<MOperand>  (VReg | PReg | Imm | Block)
            ├─ phys_uses:  Vec<PReg>   (ABI-fixed read registers)
            └─ clobbers:   Vec<PReg>   (ABI-fixed write registers)
```

`VReg` is an unlimited-supply virtual register assigned during instruction
selection. `PReg` is a physical register (target-specific numbering).

#### Register Allocator

Linear-scan register allocation (Poletto & Sarkar):

1. Compute live intervals for every `VReg`
2. Walk intervals in start-point order; expire intervals whose end < current start
3. Assign a free `PReg` from the allocatable pool, or spill the interval with the
   furthest endpoint
4. `apply_allocation` rewrites all `VReg` operands to `PReg` operands

The active set is kept sorted by interval end-point using `partition_point + insert`
for O(n log n) total (not O(n² log n)).

#### Object File Emission

`Emitter` produces a `Section` (byte stream + relocation list). The integrated
assembler stage (`IntegratedAssembler`) turns machine IR + section streams into
an object and final bytes directly (no textual assembly round-trip). `emit_object`
serializes to either ELF-64, Mach-O, or COFF depending on `ObjectFormat`:

- **ELF-64**: null + `.text` + `.symtab` + `.strtab` + `.shstrtab` + optional
  `.rela.text` section headers; uses `EM_X86_64` (62) or `EM_AARCH64` (183)
- **Mach-O**: `mach_header_64` + `LC_SEGMENT_64` + `LC_SYMTAB` + `LC_DYSYMTAB`;
  uses `CPU_TYPE_X86_64` (0x01000007) or `CPU_TYPE_ARM64` (0x0100000C)

---

### 6. `llvm-target-x86` — x86_64 Backend

#### Register File

64-bit GPRs encoded as `PReg(0)` = RAX through `PReg(15)` = R15.
- Allocatable: RAX–R11 (caller-saved, excluding RSP/RBP)
- Callee-saved: RBX, R12–R15, RBP

#### Calling Convention (System V AMD64 ABI)

- Integer args: RDI, RSI, RDX, RCX, R8, R9 (first 6); extras on stack
- Return: RAX
- Stack args at 8-byte offsets above RSP at call site

#### Instruction Selection (`lower.rs`)

`X86Backend` implements `IselBackend::lower_function`. Key design decisions:

- **2-address format**: x86 binary ops are `dst OP= src`, so each binary op first
  copies lhs into dst via `MOV_RR`, then applies the op with rhs
- **`MOV_PR` opcode**: ABI-fixed register moves store the destination PReg in
  `operands[0]` (not `dst`) so `apply_allocation` does not overwrite it
- **`emit_shift!` macro**: loads the count into RCX (CL) before the shift
- **Division**: signed uses `CQO + IDIV_R`; unsigned uses `XOR RDX,RDX + DIV_R`
- **`CondBr` edge splitting**: each conditional edge gets a trampoline machine block
  so phi-destruction copies for `then` and `else` cannot overwrite each other's
  sources (same pattern in both x86 and AArch64 backends)

#### Encoding (`encode.rs`)

Two-pass:
1. Encode all instructions, emitting 4-byte placeholder `0x00000000` for branch offsets
2. Patch `rel32` values using `target_offset − (patch_offset + 4)`

REX prefix rules: `0x48` (REX.W) for 64-bit ops; `0x41` (REX.B) for R8–R15;
bare `0x40` for SPL/BPL/SIL/DIL in byte instructions.

---

### 7. `llvm-target-arm` — AArch64 Backend

#### Register File

64-bit GPRs encoded as `PReg(0)` = X0 through `PReg(30)` = X30; `PReg(31)` = XZR.
- Allocatable: X0–X15 (caller-saved) + X19–X28 (callee-saved)
- Callee-saved: X19–X30 (X29 = frame pointer, X30 = link register)
- Arg registers: X0–X7; Return: X0

#### Calling Convention (AAPCS64)

- Integer args: X0–X7 (first 8); extras on stack at 8-byte offsets
- Return: X0

#### Instruction Selection (`lower.rs`)

`AArch64Backend` implements `IselBackend::lower_function`. Key differences from x86:

- **3-address format**: AArch64 ops have explicit `xd, xn, xm` — no pre-copy needed
- **Division**: dedicated `SDIV_RR` and `UDIV_RR` instructions (no setup required)
- **Remainder**: no native remainder instruction; emitted as `div; mul; sub`
- **`SExt`**: dispatches to `SXTB` (≤8-bit), `SXTH` (≤16-bit), `SXTW` (≤32-bit)
- **Constants**: 16-bit values use `MOV_IMM` (MOVZ); 64-bit values use `MOV_WIDE`
  (MOVZ for low 16 bits + up to three MOVK for higher chunks)

#### Encoding (`encode.rs`)

AArch64 uses fixed 32-bit instruction words. `emit4(word: u32)` appends 4 bytes
little-endian. Branch patching uses PC-relative offsets:
- `B` / `BL`: 26-bit immediate (imm26), `word |= (delta >> 2) & 0x3FFFFFF`
- `B_COND`: 19-bit immediate in bits `[23:5]`, `word |= ((delta >> 2) & 0x7FFFF) << 5`

`CSET` is encoded as `CSINC Rd, XZR, XZR, invert(cond)` = `0x9A9F17E0 | (inv_cond << 12) | Rd`.

---

### 8. `llvm-bitcode` — LRIR Binary Serialization

The **LRIR** ("LLVM-in-Rust IR") format provides compact, faithful round-trip
serialization of a `(Context, Module)` pair. It is not compatible with the upstream
LLVM bitcode (`.bc`) format; it exists to enable fast save/load of IR without
re-parsing text.

#### Wire Format

```
[4B]  magic  = "LRIR"  (0x4C 0x52 0x49 0x52)
[4B]  version = 1  (u32 LE)
[4B]  type_count  (u32 LE)
      type_count × TypeRecord
[4B]  const_count  (u32 LE)
      const_count × ConstRecord
[str] module_name
[4B]  func_count  (u32 LE)
      func_count × FunctionRecord
```

All strings: `u32 length` + UTF-8 bytes (no null terminator). Zero-length means absent.

#### Round-Trip Guarantee

`read_bitcode(write_bitcode(ctx, module))` produces a structurally equivalent
`(Context, Module)`: same types, constants, function signatures, basic-block
structure, and instruction kinds with all operands.

---

## Cross-Cutting Concerns

### SSA Invariant

The IR is always in SSA form. Every `InstrId` defines exactly one value; phi nodes
at block entries merge values from predecessor edges. The `Mem2Reg` pass converts
`alloca`/`load`/`store` patterns (typical of language frontends) to proper SSA.

### Phi-Destruction (Critical Correctness Property)

When lowering `CondBr` terminators, each conditional edge gets its own **trampoline
machine block**:

```
predecessor:
  test cond, cond
  jcc  → then_edge     (trampoline)
  jmp  → else_edge     (trampoline)

then_edge:
  mov phi_vreg, then_incoming   ← phi copy for then successor only
  jmp → then_dest

else_edge:
  mov phi_vreg, else_incoming   ← phi copy for else successor only
  jmp → else_dest
```

Without edge splitting, both sets of copies execute in the predecessor, allowing
one to overwrite a value the other needs.

### Arena Safety

No raw pointers are used. All cross-object references use `u32` newtype indices
(`TypeId`, `BlockId`, `InstrId`, `ValueRef`, …). Out-of-bounds access panics
(debug) or is UB-free (release) because all arenas grow monotonically.

---

## Phase Roadmap

| Phase | Crates | Status |
|-------|--------|--------|
| 1 — IR Foundation | `llvm-ir`, `llvm-ir-parser` | Complete |
| 2 — Analysis | `llvm-analysis` | Complete |
| 3 — Optimizations | `llvm-transforms` | Complete |
| 4 — x86_64 Backend | `llvm-codegen`, `llvm-target-x86` | Complete |
| 5 — AArch64 + Bitcode | `llvm-target-arm`, `llvm-bitcode` | Complete |
