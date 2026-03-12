# LLVM-in-Rust: User Manual

## Prerequisites

- Rust toolchain (stable, ≥ 1.65 recommended)
- Cargo (ships with Rust)
- `git`

---

## Building the Project

```bash
git clone https://github.com/yudongusa/LLVM-in-Rust.git
cd LLVM-in-Rust

# Build all crates
cargo build

# Release build (optimized)
cargo build --release

# Type-check only (fastest)
cargo check
```

---

## Running Tests

```bash
# Run the entire workspace test suite
cargo test

# Test a single crate
cargo test -p llvm-ir
cargo test -p llvm-ir-parser
cargo test -p llvm-analysis
cargo test -p llvm-transforms
cargo test -p llvm-codegen
cargo test -p llvm-target-x86
cargo test -p llvm-target-arm
cargo test -p llvm-bitcode

# Run a single test by name
cargo test lower_add_produces_machine_blocks

# Run tests with output visible
cargo test -- --nocapture
```

---

## Linting and Formatting

```bash
cargo clippy          # lint all crates
cargo fmt             # auto-format all source files
cargo fmt --check     # check formatting without modifying files
```

---

## Using the IR Builder

The `llvm-ir` crate provides a `Builder` API for constructing IR programmatically.
Add it to your `Cargo.toml`:

```toml
[dependencies]
llvm-ir = { path = "llvm-ir" }
```

### Hello World: a simple `add` function

```rust
use llvm_ir::{Builder, Context, Linkage, Module, Printer};

fn main() {
    let mut ctx = Context::new();
    let mut module = Module::new("my_module");
    let mut b = Builder::new(&mut ctx, &mut module);

    // Define: i64 @add(i64 %a, i64 %b)
    b.add_function(
        "add",
        b.ctx.i64_ty,
        vec![b.ctx.i64_ty, b.ctx.i64_ty],
        vec!["a".into(), "b".into()],
        false,           // not variadic
        Linkage::External,
    );

    let entry = b.add_block("entry");
    b.position_at_end(entry);

    let a   = b.get_arg(0);
    let bv  = b.get_arg(1);
    let sum = b.build_add("sum", a, bv);
    b.build_ret(sum);

    // Print as .ll text
    let printer = Printer::new(&ctx);
    println!("{}", printer.print_module(&module));
}
```

Output:
```llvm
define i64 @add(i64 %a, i64 %b) {
entry:
  %sum = add i64 %a, %b
  ret i64 %sum
}
```

### Building a function with control flow and phi nodes

```rust
// define i64 @max(i64 %a, i64 %b)
b.add_function("max", b.ctx.i64_ty,
    vec![b.ctx.i64_ty, b.ctx.i64_ty], vec!["a".into(), "b".into()],
    false, Linkage::External);

let entry   = b.add_block("entry");
let then_bb = b.add_block("then");
let else_bb = b.add_block("else");
let merge   = b.add_block("merge");

b.position_at_end(entry);
let a   = b.get_arg(0);
let bv  = b.get_arg(1);
let cmp = b.build_icmp("cmp", llvm_ir::IntPredicate::Sgt, a, bv);
b.build_cond_br(cmp, then_bb, else_bb);

b.position_at_end(then_bb);
b.build_br(merge);

b.position_at_end(else_bb);
b.build_br(merge);

b.position_at_end(merge);
let result = b.build_phi("result", b.ctx.i64_ty,
    vec![(a, then_bb), (bv, else_bb)]);
b.build_ret(result);
```

---

## Parsing LLVM IR Text (`.ll`)

```rust
use llvm_ir_parser::parse;

let src = r#"
define i32 @square(i32 %x) {
entry:
  %r = mul i32 %x, %x
  ret i32 %r
}
"#;

let (ctx, module) = parse(src).expect("parse error");
println!("functions: {}", module.functions.len());
```

The parser returns the same `(Context, Module)` pair as the builder, so parsed IR
can be fed directly into any analysis or optimization pass.

---

## Running Analysis Passes

```rust
use llvm_analysis::{Cfg, DomTree, UseDefInfo};

// Build the CFG for a function
let cfg = Cfg::build(&func);

// Check predecessor/successor relationships
let preds = cfg.predecessors(block_id);
let succs = cfg.successors(block_id);

// Build the dominator tree
let dom = DomTree::build(&func, &cfg);
assert!(dom.dominates(entry_block, some_block));

// Build use-def chains
let use_def = UseDefInfo::build(&func);
let users_of = use_def.uses(instr_id); // &[InstrId]
```

---

## Running Optimization Passes

```rust
use llvm_transforms::{PassManager, Mem2Reg, ConstProp, DeadCodeElim, Inliner};

let mut pm = PassManager::new();

// Add function passes (run per-function)
pm.add_function_pass(Mem2Reg);
pm.add_function_pass(ConstProp::new());
pm.add_function_pass(DeadCodeElim::new());

// Add module passes (run once per module)
pm.add_module_pass(Inliner::new());

// Run everything
pm.run(&mut ctx, &mut module);
```

Passes run in the order they are added. `Mem2Reg` should always precede `ConstProp`
and `DCE` so that alloca-based patterns are in SSA form before optimization.

---

## Generating x86_64 Machine Code

```rust
use llvm_codegen::{
    isel::IselBackend,
    regalloc::{compute_live_intervals, linear_scan, apply_allocation},
    emit::{emit_object, ObjectFormat},
};
use llvm_target_x86::lower::X86Backend;
use llvm_target_x86::encode::X86Emitter;

// Lower IR → machine IR
let mut backend = X86Backend;
let mut mf = backend.lower_function(&ctx, &module, &func);

// Register allocation
let intervals = compute_live_intervals(&mf);
let allocation = linear_scan(&intervals, &mf.allocatable_pregs);
apply_allocation(&mut mf, &allocation);

// Encode to bytes
let mut emitter = X86Emitter::new(ObjectFormat::MachO); // or ObjectFormat::Elf
let section = emitter.emit_function(&mf);

// Produce a linkable object file
let obj_bytes = emit_object(&[section], ObjectFormat::MachO, &module.functions[0].name);
std::fs::write("output.o", &obj_bytes).unwrap();
```

Link on macOS:
```bash
ld output.o -o output -lSystem -syslibroot $(xcrun --show-sdk-path) -e _main
```

Link on Linux:
```bash
ld output.o -o output -lc --dynamic-linker /lib64/ld-linux-x86-64.so.2
```

---

## Generating AArch64 Machine Code

The AArch64 backend follows the same API as x86_64, with different types:

```rust
use llvm_target_arm::lower::AArch64Backend;
use llvm_target_arm::encode::AArch64Emitter;

let mut backend = AArch64Backend;
let mut mf = backend.lower_function(&ctx, &module, &func);

let intervals  = compute_live_intervals(&mf);
let allocation = linear_scan(&intervals, &mf.allocatable_pregs);
apply_allocation(&mut mf, &allocation);

let mut emitter = AArch64Emitter::new(ObjectFormat::MachO); // or Elf
let section = emitter.emit_function(&mf);
let obj_bytes = emit_object(&[section], ObjectFormat::MachO, &func.name);
std::fs::write("output_arm64.o", &obj_bytes).unwrap();
```

---

## Saving and Loading IR (LRIR Binary Format)

```rust
use llvm_bitcode::{write_bitcode, read_bitcode};

// Serialize
let bytes = write_bitcode(&ctx, &module);
std::fs::write("my_module.lrir", &bytes).unwrap();

// Deserialize
let data = std::fs::read("my_module.lrir").unwrap();
let (ctx2, module2) = read_bitcode(&data).expect("invalid LRIR");

// The reconstructed module is structurally equivalent to the original
assert_eq!(module.functions.len(), module2.functions.len());
```

The LRIR format (`magic = "LRIR"`) is specific to this project and is not
compatible with LLVM's upstream `.bc` bitcode format.

---

## Printing IR

```rust
use llvm_ir::Printer;

let printer = Printer::new(&ctx);

// Print entire module
let text = printer.print_module(&module);
println!("{text}");

// Write to file
std::fs::write("module.ll", text).unwrap();
```

---

## Complete Pipeline Example

Below is a full example that builds IR, optimizes it, and emits a native object file.

```rust
use llvm_ir::{Builder, Context, Linkage, Module, Printer};
use llvm_transforms::{PassManager, Mem2Reg, ConstProp, DeadCodeElim};
use llvm_codegen::{
    isel::IselBackend,
    regalloc::{compute_live_intervals, linear_scan, apply_allocation},
    emit::{emit_object, ObjectFormat},
};
use llvm_target_x86::{lower::X86Backend, encode::X86Emitter};

fn main() {
    // ── 1. Build IR ──────────────────────────────────────────────────────
    let mut ctx = Context::new();
    let mut module = Module::new("example");
    let mut b = Builder::new(&mut ctx, &mut module);

    b.add_function("double", b.ctx.i64_ty,
        vec![b.ctx.i64_ty], vec!["x".into()],
        false, Linkage::External);
    let entry = b.add_block("entry");
    b.position_at_end(entry);
    let x   = b.get_arg(0);
    let two = b.build_const_int(b.ctx.i64_ty, 2, false);
    let res = b.build_mul("res", x, two);
    b.build_ret(res);

    // ── 2. Print IR before optimization ──────────────────────────────────
    let printer = Printer::new(&ctx);
    println!("=== Before optimization ===");
    println!("{}", printer.print_module(&module));

    // ── 3. Optimize ───────────────────────────────────────────────────────
    let mut pm = PassManager::new();
    pm.add_function_pass(Mem2Reg);
    pm.add_function_pass(ConstProp::new());
    pm.add_function_pass(DeadCodeElim::new());
    pm.run(&mut ctx, &mut module);

    println!("=== After optimization ===");
    println!("{}", printer.print_module(&module));

    // ── 4. Lower to x86_64 ────────────────────────────────────────────────
    let func = &module.functions[0];
    let mut backend = X86Backend;
    let mut mf = backend.lower_function(&ctx, &module, func);

    // ── 5. Register allocate ──────────────────────────────────────────────
    let intervals  = compute_live_intervals(&mf);
    let allocation = linear_scan(&intervals, &mf.allocatable_pregs);
    apply_allocation(&mut mf, &allocation);

    // ── 6. Emit object file ───────────────────────────────────────────────
    let mut emitter = X86Emitter::new(ObjectFormat::Elf);
    let section    = emitter.emit_function(&mf);
    let obj_bytes  = emit_object(&[section], ObjectFormat::Elf, &func.name);
    std::fs::write("double.o", &obj_bytes).unwrap();
    println!("Written double.o ({} bytes)", obj_bytes.len());
}
```

---

## Common Error Messages

| Error | Cause | Fix |
|-------|-------|-----|
| `parse error: unexpected token` | Malformed `.ll` syntax | Check the `.ll` file against the LLVM IR reference |
| `InvalidMagic` from `read_bitcode` | File is not LRIR format (e.g., an upstream `.bc`) | Use `llvm-ir-parser` for `.ll`; LRIR only for files written by `write_bitcode` |
| `no current function` (panic) | `Builder::add_block` called before `add_function` | Call `add_function` first |
| `no current block` (panic) | `build_*` called before `position_at_end` | Call `position_at_end(block)` before emitting instructions |
| Regalloc spills all values | Too many live values for the available register pool | Run `Mem2Reg` and `ConstProp` before lowering to reduce pressure |

---

## Project Layout Reference

```
llvm-ir/src/
  context.rs       Context, TypeId, BlockId, InstrId, ValueRef, …
  types.rs         TypeData, FloatKind, StructType, FunctionType
  value.rs         ConstantData, Argument, GlobalVariable, Linkage
  instruction.rs   Instruction, InstrKind, IntPredicate, FloatPredicate, …
  basic_block.rs   BasicBlock
  function.rs      Function
  module.rs        Module
  builder.rs       Builder
  printer.rs       Printer

llvm-ir-parser/src/
  lexer.rs         Token, Keyword, Lexer
  parser.rs        parse()

llvm-analysis/src/
  cfg.rs           Cfg::build(), predecessors(), successors()
  dominators.rs    DomTree::build(), dominates(), idom()
  loops.rs         LoopInfo::build(), Loop
  use_def.rs       UseDefInfo::build(), uses()

llvm-transforms/src/
  pass.rs          FunctionPass, ModulePass, PassManager
  mem2reg.rs       Mem2Reg
  const_prop.rs    ConstProp
  dce.rs           DeadCodeElim
  inline_pass.rs   Inliner
  constant_fold.rs try_fold()

llvm-codegen/src/
  isel.rs          VReg, PReg, MOpcode, MOperand, MInstr, MachineFunction, IselBackend
  regalloc.rs      compute_live_intervals(), linear_scan(), apply_allocation()
  emit.rs          Emitter, Section, Reloc, emit_object(), ObjectFormat

llvm-target-x86/src/
  regs.rs          RAX…R15 constants, ALLOCATABLE, CALLEE_SAVED
  abi.rs           classify_sysv_args()
  instructions.rs  MOpcode constants (MOV_RR, ADD_RR, …), CC_* codes
  lower.rs         X86Backend (IselBackend)
  encode.rs        X86Emitter (Emitter)

llvm-target-arm/src/
  regs.rs          X0…X30, XZR constants, ALLOCATABLE, ARG_REGS
  abi.rs           classify_aapcs64_args()
  instructions.rs  MOpcode constants (MOV_RR, ADD_RR, …), CC_* codes
  lower.rs         AArch64Backend (IselBackend)
  encode.rs        AArch64Emitter (Emitter)

llvm-bitcode/src/
  error.rs         BitcodeError
  writer.rs        write_bitcode()
  reader.rs        read_bitcode()
```

---

## Miscompilation Reproducer Minimization Utility

Use `llvm-ir-min` to reduce a failing `.ll` reproducer while preserving a failure predicate.

```bash
# Build/run minimizer
cargo run -p llvm --bin llvm-ir-min -- \
  --input failing.ll \
  --predicate './repro.sh {{input}}' \
  --output minimized.ll
```

Notes:
- `{{input}}` in `--predicate` is replaced with the candidate IR path.
- Predicate must return **non-zero** when the bug is still reproduced.
- The tool performs line-based reduction and writes `minimized.ll`.
- Pair this output with the miscompilation issue template evidence package.
