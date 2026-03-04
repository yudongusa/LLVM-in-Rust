#![feature(test)]
extern crate test;

use llvm_codegen::{
    emit_object,
    isel::IselBackend,
    regalloc::{apply_allocation, compute_live_intervals, insert_spill_reloads, linear_scan},
    ObjectFormat,
};
use llvm_ir::{Builder, Context, Linkage, Module, Printer};
use llvm_ir_parser::parser::parse;
use llvm_target_x86::{
    instructions::{MOV_LOAD_MR, MOV_STORE_RM},
    X86Backend, X86Emitter,
};
use llvm_transforms::{pass::PassManager, DeadCodeElim, Mem2Reg};
use test::Bencher;

const FIXTURE: &str = include_str!("../fixtures/sample.ll");

fn parsed_module() -> (Context, Module) {
    parse(FIXTURE).expect("fixture must parse")
}

/// Run the full codegen pipeline for every function in the module.
fn codegen_module(ctx: &Context, module: &Module) {
    let mut backend = X86Backend;
    for func in &module.functions {
        if func.is_declaration {
            continue;
        }
        let mut mf = backend.lower_function(ctx, module, func);
        let intervals = compute_live_intervals(&mf);
        let mut result = linear_scan(&intervals, &mf.allocatable_pregs);
        insert_spill_reloads(&mut mf, &mut result, MOV_LOAD_MR, MOV_STORE_RM);
        apply_allocation(&mut mf, &result);
        let mut emitter = X86Emitter::new(ObjectFormat::Elf);
        emit_object(&mf, &mut emitter);
    }
}

// ── benchmarks ───────────────────────────────────────────────────────────────

#[bench]
fn bench_parse(b: &mut Bencher) {
    b.bytes = FIXTURE.len() as u64;
    b.iter(|| parse(test::black_box(FIXTURE)).unwrap());
}

#[bench]
fn bench_print(b: &mut Bencher) {
    let (ctx, module) = parsed_module();
    b.iter(|| {
        let p = Printer::new(test::black_box(&ctx));
        p.print_module(test::black_box(&module))
    });
}

#[bench]
fn bench_build(b: &mut Bencher) {
    b.iter(|| {
        let mut ctx = Context::new();
        let mut module = Module::new("built");
        let mut bldr = Builder::new(&mut ctx, &mut module);
        bldr.add_function(
            "add",
            bldr.ctx.i64_ty,
            vec![bldr.ctx.i64_ty, bldr.ctx.i64_ty],
            vec!["a".into(), "b".into()],
            false,
            Linkage::External,
        );
        let entry = bldr.add_block("entry");
        bldr.position_at_end(entry);
        let a = bldr.get_arg(0);
        let bv = bldr.get_arg(1);
        let s = bldr.build_add("s", a, bv);
        bldr.build_ret(s);
        test::black_box((ctx, module))
    });
}

#[bench]
fn bench_mem2reg(b: &mut Bencher) {
    b.iter(|| {
        let (mut ctx, mut module) = parsed_module();
        let mut pm = PassManager::new();
        pm.add_function_pass(Mem2Reg);
        pm.run(test::black_box(&mut ctx), test::black_box(&mut module));
        test::black_box((ctx, module))
    });
}

#[bench]
fn bench_dce(b: &mut Bencher) {
    b.iter(|| {
        let (mut ctx, mut module) = parsed_module();
        let mut pm = PassManager::new();
        pm.add_function_pass(DeadCodeElim);
        pm.run(test::black_box(&mut ctx), test::black_box(&mut module));
        test::black_box((ctx, module))
    });
}

#[bench]
fn bench_codegen_x86(b: &mut Bencher) {
    let (ctx, module) = parsed_module();
    b.iter(|| codegen_module(test::black_box(&ctx), test::black_box(&module)));
}
