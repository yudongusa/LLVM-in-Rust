//! tikv_jit — example: LLVM-in-Rust as a JIT backend for expression evaluation.
//!
//! This example shows how a project like TiKV could use LLVM-in-Rust to
//! JIT-compile coprocessor filter predicates for key-value range scans,
//! without any dependency on the real LLVM C++ library.
//!
//! # The compiled function
//!
//! We build the following logic in SSA IR:
//!
//! ```c
//! // Coprocessor predicate: return (value - threshold) when value > threshold, else 0.
//! // This models a "clipped difference" range filter.
//! i64 eval_predicate(i64 value, i64 threshold) {
//!     if (value > threshold)
//!         return value - threshold;
//!     else
//!         return 0;
//! }
//! ```
//!
//! # Pipeline
//!
//! 1. Programmatically build LLVM IR with the `Builder` API.
//! 2. Print the IR to a `.ll` string (for inspection / logging).
//! 3. Run an optimization preset pipeline selected by `--opt-level`.
//! 4. Run the x86-64 instruction-selection + register-allocation + spill pipeline.
//! 5. Emit an ELF `.o` file that can be linked into a Rust binary.

use std::fs;

use llvm_codegen::{
    emit_object,
    isel::IselBackend,
    regalloc::{apply_allocation, compute_live_intervals, insert_spill_reloads, linear_scan},
    ObjectFormat,
};
use llvm_ir::{Builder, Context, IntPredicate, Linkage, Module, Printer};
use llvm_target_x86::{
    instructions::{MOV_LOAD_MR, MOV_STORE_RM},
    X86Backend, X86Emitter,
};
use llvm_transforms::{build_pipeline, OptLevel};

fn parse_opt_level() -> OptLevel {
    let mut args = std::env::args().skip(1);
    let mut level = OptLevel::O2;
    while let Some(arg) = args.next() {
        if arg == "--opt-level" {
            if let Some(v) = args.next() {
                level = OptLevel::parse(&v).unwrap_or_else(|| {
                    eprintln!("invalid --opt-level '{v}' (expected O0/O1/O2/O3 or 0/1/2/3)");
                    std::process::exit(2);
                });
            } else {
                eprintln!("missing value for --opt-level");
                std::process::exit(2);
            }
        }
    }
    level
}

fn main() {
    let opt_level = parse_opt_level();
    println!("Using optimization level: {:?}", opt_level);

    // ── Step 1: Build IR ──────────────────────────────────────────────────────

    let mut ctx = Context::new();
    let mut module = Module::new("tikv_coprocessor");

    let i64_ty = ctx.i64_ty;

    {
        let mut bldr = Builder::new(&mut ctx, &mut module);

        // declare i64 @eval_predicate(i64 %value, i64 %threshold)
        bldr.add_function(
            "eval_predicate",
            i64_ty,
            vec![i64_ty, i64_ty],
            vec!["value".into(), "threshold".into()],
            false,
            Linkage::External,
        );

        // entry:
        //   %cond = icmp sgt i64 %value, %threshold
        //   br i1 %cond, label %then, label %else
        let entry = bldr.add_block("entry");
        bldr.position_at_end(entry);
        let value = bldr.get_arg(0);
        let threshold = bldr.get_arg(1);
        let cond = bldr.build_icmp("cond", IntPredicate::Sgt, value, threshold);
        let then_bb = bldr.add_block("then");
        let else_bb = bldr.add_block("else");
        bldr.build_cond_br(cond, then_bb, else_bb);

        // then:
        //   %diff = sub i64 %value, %threshold
        //   br label %merge
        let merge_bb = bldr.add_block("merge");
        bldr.position_at_end(then_bb);
        let diff = bldr.build_sub("diff", value, threshold);
        bldr.build_br(merge_bb);

        // else:
        //   br label %merge
        bldr.position_at_end(else_bb);
        bldr.build_br(merge_bb);

        // merge:
        //   %result = phi i64 [ %diff, %then ], [ 0, %else ]
        //   ret i64 %result
        bldr.position_at_end(merge_bb);
        let zero = bldr.const_i64(0);
        let result = bldr.build_phi("result", i64_ty, vec![(diff, then_bb), (zero, else_bb)]);
        bldr.build_ret(result);
    }

    // ── Step 2: Print IR ──────────────────────────────────────────────────────

    let ir_text = Printer::new(&ctx).print_module(&module);
    println!("=== LLVM IR (before optimisation) ===");
    println!("{ir_text}");

    // ── Step 3: Optimise ──────────────────────────────────────────────────────

    let mut pm = build_pipeline(opt_level);
    pm.run_until_fixed_point(&mut ctx, &mut module, 8);

    let ir_opt = Printer::new(&ctx).print_module(&module);
    println!("=== LLVM IR (after optimisation) ===");
    println!("{ir_opt}");

    // ── Step 4: x86-64 codegen ────────────────────────────────────────────────

    let func = module
        .functions
        .iter()
        .find(|f| !f.is_declaration)
        .expect("module has at least one function definition");

    let mut backend = X86Backend;
    let mut mf = backend.lower_function(&ctx, &module, func);

    let intervals = compute_live_intervals(&mf);
    let mut result = linear_scan(&intervals, &mf.allocatable_pregs);
    insert_spill_reloads(&mut mf, &mut result, MOV_LOAD_MR, MOV_STORE_RM);
    apply_allocation(&mut mf, &result);

    // ── Step 5: Emit ELF object file ──────────────────────────────────────────

    let mut emitter = X86Emitter::new(ObjectFormat::Elf);
    let obj = emit_object(&mf, &mut emitter);
    let bytes = obj.to_bytes();

    let out_path = "/tmp/eval_predicate.o";
    fs::write(out_path, &bytes).expect("failed to write object file");

    println!("=== Object file ===");
    println!("Wrote {} bytes to {out_path}", bytes.len());
    println!();
    println!("Link with:  cc /tmp/eval_predicate.o -o /tmp/eval_predicate_test your_main.o");
    println!("Or inspect: objdump -d {out_path}");
}
