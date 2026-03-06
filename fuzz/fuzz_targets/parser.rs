#![no_main]

use libfuzzer_sys::fuzz_target;
use llvm_codegen::{
    emit_object,
    isel::IselBackend,
    regalloc::{apply_allocation, compute_live_intervals, insert_spill_reloads, linear_scan},
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
        let mut emitter = X86Emitter::new(obj_format);
        let _ = emit_object(&mf, &mut emitter).to_bytes();
    }
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

    // Phase 2 in issue #82: exercise optimizer passes.
    let mut pm = PassManager::new();
    pm.add_function_pass(Mem2Reg);
    pm.add_function_pass(DeadCodeElim);
    pm.run(&mut ctx, &mut module);

    // Phase 3 in issue #82: exercise codegen pipeline and object emission.
    run_codegen(&module, &ctx);
});
