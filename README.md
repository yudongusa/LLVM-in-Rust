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

## Contributing

1. Fork the repository and create a feature branch.
2. Make your changes; ensure `cargo clippy --all-targets` is warning-free and `cargo fmt --check` passes.
3. Add tests covering new behaviour.
4. Open a pull request against `main`.

Bug reports and feature requests go to the [issue tracker](https://github.com/yudongusa/LLVM-in-Rust/issues).

---

## License

Licensed under the [Apache License, Version 2.0](LICENSE).
