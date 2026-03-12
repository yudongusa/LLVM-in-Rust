#![no_main]

use libfuzzer_sys::fuzz_target;
use llvm_codegen::{
    emit_object,
    isel::IselBackend,
    regalloc::{
        allocate_registers, apply_allocation, compute_live_intervals, insert_spill_reloads,
        RegAllocStrategy,
    },
    ObjectFormat,
};
use llvm_ir::Module;
use llvm_ir_parser::parser::parse;
use llvm_target_x86::{
    instructions::{MOV_LOAD_MR, MOV_STORE_RM},
    X86Backend, X86Emitter,
};
use llvm_transforms::{pass::PassManager, DeadCodeElim, Mem2Reg};

fn host_object_format() -> Option<ObjectFormat> {
    if cfg!(target_os = "macos") {
        Some(ObjectFormat::MachO)
    } else if cfg!(target_os = "linux") {
        Some(ObjectFormat::Elf)
    } else {
        None
    }
}

fn run_codegen(module: &Module, ctx: &llvm_ir::Context) {
    let obj_format = match host_object_format() {
        Some(f) => f,
        None => return,
    };

    let mut backend = X86Backend::default();
    for func in &module.functions {
        if func.is_declaration {
            continue;
        }
        let mut mf = backend.lower_function(ctx, module, func);
        let intervals = compute_live_intervals(&mf);
        let mut result = allocate_registers(
            &intervals,
            &mf.allocatable_pregs,
            RegAllocStrategy::LinearScan,
        );
        insert_spill_reloads(&mut mf, &mut result, MOV_LOAD_MR, MOV_STORE_RM);
        apply_allocation(&mut mf, &result);
        let mut emitter = X86Emitter::new(obj_format);
        let _ = emit_object(&mf, &mut emitter).to_bytes();
    }
}

fn within_complexity_budget(module: &Module) -> bool {
    // Keep fuzzing signal strong while avoiding pathological workloads that
    // turn this harness into a long-running compiler benchmark.
    const MAX_FUNCTIONS: usize = 256;
    const MAX_BLOCKS: usize = 4096;
    const MAX_INSTRUCTIONS: usize = 200_000;

    if module.functions.len() > MAX_FUNCTIONS {
        return false;
    }

    let mut blocks = 0usize;
    let mut instrs = 0usize;
    for f in &module.functions {
        blocks = blocks.saturating_add(f.blocks.len());
        for bb in &f.blocks {
            instrs = instrs.saturating_add(bb.instrs.len());
        }
    }

    blocks <= MAX_BLOCKS && instrs <= MAX_INSTRUCTIONS
}

fuzz_target!(|data: &[u8]| {
    if data.len() > 256 * 1024 {
        return;
    }

    let src = match std::str::from_utf8(data) {
        Ok(s) => s,
        Err(_) => return,
    };

    let (mut ctx, mut module) = match parse(src) {
        Ok(m) => m,
        Err(_) => return,
    };

    if !within_complexity_budget(&module) {
        return;
    }

    // Phase 2 in issue #82: exercise optimizer passes.
    let mut pm = PassManager::new();
    pm.add_function_pass(Mem2Reg);
    pm.add_function_pass(DeadCodeElim);
    pm.run(&mut ctx, &mut module);

    // Phase 3 in issue #82: exercise codegen pipeline and object emission.
    run_codegen(&module, &ctx);
});
