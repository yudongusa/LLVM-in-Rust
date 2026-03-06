use std::hint::black_box;

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
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
use llvm_transforms::{build_pipeline, pass::PassManager, DeadCodeElim, Mem2Reg, OptLevel};

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

fn bench_parse(c: &mut Criterion) {
    let mut group = c.benchmark_group("pipeline");
    group.throughput(Throughput::Bytes(FIXTURE.len() as u64));
    group.bench_function("parse", |b| b.iter(|| parse(black_box(FIXTURE)).unwrap()));
    group.finish();
}

fn bench_print(c: &mut Criterion) {
    let (ctx, module) = parsed_module();
    c.bench_function("pipeline/print", |b| {
        b.iter(|| {
            let p = Printer::new(black_box(&ctx));
            p.print_module(black_box(&module))
        })
    });
}

fn bench_build(c: &mut Criterion) {
    c.bench_function("pipeline/build", |b| {
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
            black_box((ctx, module))
        })
    });
}

fn bench_mem2reg(c: &mut Criterion) {
    c.bench_function("pipeline/mem2reg", |b| {
        b.iter(|| {
            let (mut ctx, mut module) = parsed_module();
            let mut pm = PassManager::new();
            pm.add_function_pass(Mem2Reg);
            pm.run(black_box(&mut ctx), black_box(&mut module));
            black_box((ctx, module))
        })
    });
}

fn bench_dce(c: &mut Criterion) {
    c.bench_function("pipeline/dce", |b| {
        b.iter(|| {
            let (mut ctx, mut module) = parsed_module();
            let mut pm = PassManager::new();
            pm.add_function_pass(DeadCodeElim);
            pm.run(black_box(&mut ctx), black_box(&mut module));
            black_box((ctx, module))
        })
    });
}

fn bench_codegen_x86(c: &mut Criterion) {
    let (ctx, module) = parsed_module();
    c.bench_function("pipeline/codegen_x86", |b| {
        b.iter(|| codegen_module(black_box(&ctx), black_box(&module)))
    });
}

fn bench_opt_o0(c: &mut Criterion) {
    c.bench_function("pipeline/opt_O0", |b| {
        b.iter(|| {
            let (mut ctx, mut module) = parsed_module();
            let mut pm = build_pipeline(OptLevel::O0);
            pm.run_until_fixed_point(black_box(&mut ctx), black_box(&mut module), 8);
            black_box((ctx, module))
        })
    });
}

fn bench_opt_o2(c: &mut Criterion) {
    c.bench_function("pipeline/opt_O2", |b| {
        b.iter(|| {
            let (mut ctx, mut module) = parsed_module();
            let mut pm = build_pipeline(OptLevel::O2);
            pm.run_until_fixed_point(black_box(&mut ctx), black_box(&mut module), 8);
            black_box((ctx, module))
        })
    });
}

criterion_group!(
    pipeline,
    bench_parse,
    bench_print,
    bench_build,
    bench_mem2reg,
    bench_dce,
    bench_codegen_x86,
    bench_opt_o0,
    bench_opt_o2
);
criterion_main!(pipeline);
