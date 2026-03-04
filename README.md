# LLVM-in-Rust

A pure Rust re-implementation of LLVM — no C++, no FFI, no `llvm-sys`. The full compiler pipeline from LLVM IR through optimization passes to machine code generation is implemented entirely in safe Rust.

## Why?

The official LLVM is a C++ library that Rust projects consume through a fragile C FFI wrapper (`llvm-sys`). This project explores what a clean, idiomatic Rust implementation of the same pipeline looks like: arena-free ownership, type-safe index handles, trait-based pass infrastructure, and zero unsafe code in the core IR.

## Status

All of Phase 1–5 are implemented and tested (196 tests, all passing):

| Phase | What | Status |
|-------|------|--------|
| 1 | IR foundation — types, values, instructions, builder, printer, `.ll` parser | Done |
| 2 | Analysis — CFG, dominator tree, use-def chains, loop detection | Done |
| 3 | Optimization — mem2reg, DCE, constant folding/propagation, inlining | Done |
| 4 | x86_64 backend — instruction selection, register allocation, ELF/Mach-O emission | Done |
| 5 | AArch64 backend + binary IR format (LRIR) reader/writer | Done |

---

## Performance

Benchmarks compare this project against LLVM 19.1.7 (Homebrew) on a 15-function
representative module (`src/llvm-bench/fixtures/sample.ll`, ~340 lines, integer/FP/memory ops).

Run the benchmarks yourself:

```bash
cargo bench -p llvm-bench
```

### Results (x86_64 macOS, Apple M-series, release build)

| Pipeline stage | This project | LLVM 19 tool | LLVM 19 (processing only¹) |
|---|---|---|---|
| Parse `.ll` → IR | **183 µs** | `llvm-as`: 116 ms wall | ~36 ms |
| Print IR → `.ll` | **33 µs** | `llvm-dis`: 82 ms wall | ~2 ms |
| mem2reg pass | **80 µs**² | `opt -passes=mem2reg`: 98 ms wall | ~10 ms |
| DCE pass | **55 µs**² | `opt -passes=dce`: 90 ms wall | ~2 ms |
| x86_64 codegen | **116 µs** | `llc -O0`: 108 ms wall | ~18 ms |
| Builder API (2 fns) | **2.4 µs** | — | — |

¹ LLVM tool wall-clock includes ~80–90 ms process startup + dynamic library loading.
  "Processing only" subtracts the baseline measured with a trivial single-function input.
  This makes the comparison more representative of in-process library use.

² Mem2reg and DCE benchmarks include parsing the fixture on each iteration;
  net pass-only time is wall time minus the 183 µs parse cost.

### Interpretation

- **This project runs in-process with zero startup cost**, which explains most of the
  wall-clock advantage. LLVM tools (`llvm-as`, `opt`, `llc`) pay 80–90 ms every invocation
  just to load the shared libraries — dwarfing the actual work on a small module.

- **Processing-time comparison**: even after subtracting startup overhead, this implementation
  is meaningfully faster for small-to-medium modules (~5–125×). The primary reasons:
  - Focused implementation without LLVM's plugin, debug, metadata, and attribute infrastructure
  - Vec-based flat arenas vs. LLVM's layered allocator hierarchy
  - No LLVM pass-manager bookkeeping (analyses, invalidation, statistics)

- **Scalability caveat**: LLVM is highly optimised for large programs (hundreds of thousands
  of IR instructions). At that scale LLVM's mature optimisations will outperform this project.
  These benchmarks target the small-module embedded-library use case.

- **Code quality**: this project does not attempt to produce code as optimised as LLVM `-O2`.
  The codegen benchmarks compare `-O0` (unoptimised) compilation speed only.

---

## LLVM IR compatibility

This project is a **standalone re-implementation**, not a wrapper around LLVM. "LLVM compatibility" means compatibility with the LLVM IR text format (`.ll` files) that real LLVM tools (`clang`, `opt`, `llc`, `llvm-as`) read and write.

### Supported LLVM versions: 15 – 22

| LLVM release | `.ll` files this project emits | `.ll` files from that LLVM version |
|---|---|---|
| ≤ 14 | Not readable by LLVM ≤14 (opaque-pointer syntax) | **Not parseable** (typed pointer syntax unsupported) |
| 15 | Compatible | Compatible (opaque pointers on by default) |
| 16 | Compatible | Compatible |
| 17 | Compatible | Compatible |
| 18 – 22 | Compatible | Compatible |

**Current LLVM stable release:** 22.1.0 (February 2026)

### Key IR feature versions

| IR feature | This project | First LLVM version |
|---|---|---|
| Opaque pointers (`ptr`) | **Yes** — only pointer representation | Default in LLVM 15; exclusive in LLVM 17 |
| Typed pointers (`i32*`, `float*`) | **No** — not supported | Removed in LLVM 17 |
| `bfloat` (BFloat16) type | **Yes** | LLVM 7 |
| Scalable vectors (`<vscale x N x T>`) | **Yes** | LLVM 11 (SVE/RVV) |
| `nuw` / `nsw` / `exact` flags | **Yes** | LLVM 2.9 |
| Fast-math flags (`nnan`, `ninf`, `fast`, …) | **Yes** | LLVM 3.1 |
| Tail-call kinds (`tail`, `musttail`, `notail`) | **Yes** | LLVM 3.0 |
| `fneg` instruction | **Yes** | LLVM 9 |
| `freeze` instruction | **No** | LLVM 10 |
| `vp.*` vector-predication intrinsics | **No** | LLVM 11 |

### What this means in practice

- **Reading LLVM 15+ output:** `.ll` files generated by `clang -S -emit-llvm` with LLVM 15 or
  later can be parsed directly by `llvm-ir-parser`.
- **Reading LLVM ≤14 output:** Not supported. LLVM 14 and earlier emit typed-pointer syntax
  (`i32*`, `i8**`) that this parser does not recognise. Pass those files through
  `opt --opaque-pointers -S` with LLVM 15 to upgrade them first.
- **Interop with LLVM tools:** `.ll` files printed by `llvm_ir::Printer` are valid input to
  `opt`, `llc`, and `llvm-as` version 15 or later.

### Binary format (LRIR) — not LLVM bitcode

The `llvm-bitcode` crate implements a **custom compact binary format called LRIR**, identified
by the magic bytes `LRIR` and format version 1. This is **not** the LLVM bitcode format (`.bc`)
and cannot be processed by LLVM tools (`llvm-as`, `llvm-dis`, `llvm-link`). LRIR exists solely
for fast round-trip serialization within this project. Use the text format (`.ll`) for
interoperability with external LLVM tools.

---

## Crate layout

```
llvm-ir/          Core IR types: types, values, instructions, modules, builder, printer
llvm-ir-parser/   .ll text format parser
llvm-analysis/    CFG, dominator tree, use-def chains, loop info
llvm-transforms/  Optimization passes: mem2reg, DCE, const folding/prop, inliner
llvm-codegen/     Target-independent codegen: legalization, isel, regalloc, scheduling
llvm-target-x86/  x86_64 backend
llvm-target-arm/  AArch64 backend
llvm-bitcode/     Binary IR format (LRIR) reader/writer
llvm/             Top-level crate re-exporting everything
```

Crate dependency graph (arrows = "depends on"):

```
llvm-ir-parser ──┐
llvm-analysis  ──┤
                 ├──► llvm-ir
llvm-transforms──┤
llvm-codegen   ──┘
    │
    ├──► llvm-target-x86
    └──► llvm-target-arm
llvm-bitcode ──► llvm-ir
llvm ──► all of the above
```

---

## Prerequisites

- Rust 1.75 or later (2021 edition)
- Cargo (ships with Rust)

Install Rust via [rustup](https://rustup.rs/):

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

---

## Building

```bash
# Clone the repository
git clone https://github.com/yudongusa/LLVM-in-Rust.git
cd LLVM-in-Rust

# Debug build (fast compile, includes assertions)
cargo build

# Release build (optimised)
cargo build --release

# Type-check only (fastest feedback loop)
cargo check
```

### Build a specific crate

```bash
cargo build -p llvm-ir
cargo build -p llvm-transforms
cargo build -p llvm-target-x86
```

---

## Testing

```bash
# Run all tests
cargo test

# Run tests for a single crate
cargo test -p llvm-ir
cargo test -p llvm-ir-parser
cargo test -p llvm-analysis
cargo test -p llvm-transforms
cargo test -p llvm-codegen
cargo test -p llvm-target-x86
cargo test -p llvm-target-arm
cargo test -p llvm-bitcode

# Run a named test
cargo test roundtrip_add
cargo test mem2reg_simple_alloca
```

### Linting and formatting

```bash
cargo clippy --all-targets   # must be warning-free
cargo fmt --check            # check formatting
cargo fmt                    # auto-format
```

---

## Installation

This project is a Cargo workspace of library crates. Add whichever layers you need to your project's `Cargo.toml`:

```toml
[dependencies]
# Core IR types only
llvm-ir = { git = "https://github.com/yudongusa/LLVM-in-Rust" }

# IR + .ll text parser
llvm-ir-parser = { git = "https://github.com/yudongusa/LLVM-in-Rust" }

# IR + optimization passes
llvm-transforms = { git = "https://github.com/yudongusa/LLVM-in-Rust" }

# Full pipeline including x86_64 backend
llvm = { git = "https://github.com/yudongusa/LLVM-in-Rust" }
```

For a path dependency (local development):

```toml
[dependencies]
llvm-ir = { path = "../LLVM-in-Rust/src/llvm-ir" }
```

---

## Usage

### Build IR programmatically

```rust
use llvm_ir::{Builder, Context, IntPredicate, Linkage, Module, Printer};

fn main() {
    let mut ctx = Context::new();
    let mut module = Module::new("example");
    let mut b = Builder::new(&mut ctx, &mut module);

    // define i32 @max(i32 %x, i32 %y)
    let _fid = b.add_function(
        "max",
        b.ctx.i32_ty,
        vec![b.ctx.i32_ty, b.ctx.i32_ty],
        vec!["x".into(), "y".into()],
        false,
        Linkage::External,
    );

    let entry   = b.add_block("entry");
    let ret_x   = b.add_block("ret_x");
    let ret_y   = b.add_block("ret_y");

    b.position_at_end(entry);
    let x    = b.get_arg(0);
    let y    = b.get_arg(1);
    let cond = b.build_icmp("cond", IntPredicate::Sgt, x, y);
    b.build_cond_br(cond, ret_x, ret_y);

    b.position_at_end(ret_x);
    b.build_ret(x);

    b.position_at_end(ret_y);
    b.build_ret(y);

    // Print LLVM IR text
    let ir = Printer::new(b.ctx).print_module(b.module);
    println!("{ir}");
}
```

Output:

```llvm
; Module: example

define i32 @max(i32 %x, i32 %y) {
entry:
  %cond = icmp sgt i32 %x, %y
  br i1 %cond, label %ret_x, label %ret_y
ret_x:
  ret i32 %x
ret_y:
  ret i32 %y
}
```

### Parse a `.ll` file

```rust
use llvm_ir_parser::parse;
use llvm_ir::Printer;

fn main() {
    let src = std::fs::read_to_string("input.ll").unwrap();
    let (ctx, module) = parse(&src).expect("parse error");

    // Round-trip back to text
    let ir = Printer::new(&ctx).print_module(&module);
    println!("{ir}");
}
```

### Run optimization passes

```rust
use llvm_ir::{Builder, Context, Linkage, Module};
use llvm_transforms::{
    ConstProp, DeadCodeElim, Inliner, Mem2Reg,
    FunctionPass, ModulePass, PassManager,
    pass::FunctionPassAdapter,
};

fn main() {
    // ... build or parse your module into (ctx, module) ...

    let mut pm = PassManager::new();
    pm.add(Box::new(FunctionPassAdapter { pass: Mem2Reg }));
    pm.add(Box::new(FunctionPassAdapter { pass: ConstProp }));
    pm.add(Box::new(FunctionPassAdapter { pass: DeadCodeElim }));
    pm.add(Box::new(Inliner::new(/* size_limit */ 100)));

    pm.run(&mut ctx, &mut module); // runs to fixed-point
}
```

### Emit x86_64 machine code

```rust
use llvm_ir::{Builder, Context, Linkage, Module};
use llvm_codegen::{emit_object, ObjectFormat};
use llvm_target_x86::X86Backend;

fn main() {
    // ... build or parse your module ...

    let backend = X86Backend::new();
    let obj = emit_object(&ctx, &module, &backend, ObjectFormat::Elf)
        .expect("codegen failed");

    std::fs::write("output.o", &obj.bytes).unwrap();
}
```

### Save/load binary IR (LRIR format)

```rust
use llvm_bitcode::{read_bitcode, write_bitcode};

// Serialise
let bytes = write_bitcode(&ctx, &module).unwrap();
std::fs::write("module.lrir", &bytes).unwrap();

// Deserialise
let bytes = std::fs::read("module.lrir").unwrap();
let (ctx, module) = read_bitcode(&bytes).unwrap();
```

---

## Architecture overview

### IR design

All IR lives in SSA form. Ownership is structured around three stack values:

- **`Context`** — interned type table and constant pool
- **`Module`** — functions and global variables
- **`Function`** — flat instruction pool; `BasicBlock` stores `Vec<InstrId>` indices into it

All handles (`TypeId`, `BlockId`, `InstrId`, `ArgId`, `ConstId`, `GlobalId`, `FunctionId`) are `Copy` `u32` newtypes. Cross-entity references use `ValueRef`, a `Copy` enum:

```rust
pub enum ValueRef {
    Instruction(InstrId),
    Argument(ArgId),
    Constant(ConstId),
    Global(GlobalId),
}
```

There is no `Rc`, no `RefCell`, no unsafe, and no external arena crate.

### Pass infrastructure

`FunctionPass` and `ModulePass` are simple traits:

```rust
pub trait FunctionPass {
    fn run_on_function(&mut self, ctx: &mut Context, func: &mut Function) -> bool;
    fn name(&self) -> &'static str;
}
```

`FunctionPassAdapter` lifts any `FunctionPass` into a `ModulePass`. `PassManager` sequences passes and iterates until no pass reports a change.

### Code generation

Target backends implement `IselBackend`. The pipeline is:

```
IR  →  legalize  →  instruction selection (DAG lowering)
    →  register allocation (linear scan)
    →  instruction scheduling
    →  machine-code encoding  →  ELF / Mach-O object file
```

---

## Usage example: embedding as a JIT backend

The [`examples/tikv_jit`](examples/tikv_jit) crate shows how a project like
[TiKV](https://github.com/tikv/tikv) could embed LLVM-in-Rust to JIT-compile
coprocessor filter predicates — with **no dependency on the LLVM C++ library**.

### What it compiles

```c
// Clipped-difference range filter: return (value - threshold) when value > threshold, else 0.
i64 eval_predicate(i64 value, i64 threshold) {
    if (value > threshold) return value - threshold;
    return 0;
}
```

### Full pipeline in ~60 lines of Rust

```rust
use llvm_codegen::{
    emit_object, isel::IselBackend, ObjectFormat,
    regalloc::{apply_allocation, compute_live_intervals, insert_spill_reloads, linear_scan},
};
use llvm_ir::{Builder, Context, IntPredicate, Linkage, Module, Printer};
use llvm_target_x86::{instructions::{MOV_LOAD_MR, MOV_STORE_RM}, X86Backend, X86Emitter};
use llvm_transforms::{pass::PassManager, ConstProp, DeadCodeElim, Mem2Reg};

// 1. Build IR
let mut ctx = Context::new();
let mut module = Module::new("tikv_coprocessor");
let i64_ty = ctx.i64_ty;
{
    let mut bldr = Builder::new(&mut ctx, &mut module);
    bldr.add_function("eval_predicate", i64_ty,
        vec![i64_ty, i64_ty], vec!["value".into(), "threshold".into()],
        false, Linkage::External);

    let entry = bldr.add_block("entry");
    bldr.position_at_end(entry);
    let value = bldr.get_arg(0);
    let threshold = bldr.get_arg(1);
    let cond = bldr.build_icmp("cond", IntPredicate::Sgt, value, threshold);
    let then_bb = bldr.add_block("then");
    let else_bb = bldr.add_block("else");
    bldr.build_cond_br(cond, then_bb, else_bb);

    let merge_bb = bldr.add_block("merge");
    bldr.position_at_end(then_bb);
    let diff = bldr.build_sub("diff", value, threshold);
    bldr.build_br(merge_bb);

    bldr.position_at_end(else_bb);
    bldr.build_br(merge_bb);

    bldr.position_at_end(merge_bb);
    let zero = bldr.const_i64(0);
    let result = bldr.build_phi("result", i64_ty, vec![(diff, then_bb), (zero, else_bb)]);
    bldr.build_ret(result);
}

// 2. Print IR (optional — for logging / debugging)
let ir_text = Printer::new(&ctx).print_module(&module);

// 3. Optimise
let mut pm = PassManager::new();
pm.add_function_pass(Mem2Reg);
pm.add_function_pass(ConstProp);
pm.add_function_pass(DeadCodeElim);
pm.run(&mut ctx, &mut module);

// 4. x86-64 codegen
let func = module.functions.iter().find(|f| !f.is_declaration).unwrap();
let mut mf = X86Backend.lower_function(&ctx, &module, func);
let intervals = compute_live_intervals(&mf);
let mut result = linear_scan(&intervals, &mf.allocatable_pregs);
insert_spill_reloads(&mut mf, &mut result, MOV_LOAD_MR, MOV_STORE_RM);
apply_allocation(&mut mf, &result);

// 5. Emit ELF object file
let mut emitter = X86Emitter::new(ObjectFormat::Elf);
let obj = emit_object(&mf, &mut emitter);
std::fs::write("/tmp/eval_predicate.o", obj.to_bytes()).unwrap();
// Link with: cc /tmp/eval_predicate.o your_main.o -o binary
```

### Running the example

```bash
cargo run -p tikv_jit
# Prints the IR before/after optimisation, then writes /tmp/eval_predicate.o
objdump -d /tmp/eval_predicate.o   # inspect generated x86-64 assembly
```

### Adding to your own project

Add the crates you need to your `Cargo.toml`. For a local checkout use path dependencies:

```toml
[dependencies]
llvm-ir         = { path = "path/to/LLVM-in-Rust/src/llvm-ir" }
llvm-transforms = { path = "path/to/LLVM-in-Rust/src/llvm-transforms" }
llvm-codegen    = { path = "path/to/LLVM-in-Rust/src/llvm-codegen" }
llvm-target-x86 = { path = "path/to/LLVM-in-Rust/src/llvm-target-x86" }
# optional: llvm-target-arm  for AArch64 output
# optional: llvm-ir-parser   to accept .ll text files as input
# optional: llvm-bitcode     to read/write the LRIR binary format
```

---

## Contributing

1. Fork the repository and create a feature branch.
2. Make your changes; ensure `cargo clippy --all-targets` is warning-free and `cargo fmt --check` passes.
3. Add tests covering new behaviour.
4. Open a pull request against `main`.

Bug reports and feature requests go to the [issue tracker](https://github.com/yudongusa/LLVM-in-Rust/issues).

---

## License

Licensed under the [Apache License, Version 2.0](LICENSE).
