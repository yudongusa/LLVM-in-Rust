use std::path::Path;
use std::process::Command;

use llvm_codegen::{
    emit_object,
    isel::IselBackend,
    regalloc::{
        allocate_registers, apply_allocation, compute_live_intervals, insert_spill_reloads,
        RegAllocStrategy,
    },
    ObjectFormat,
};
use llvm_ir::{Context, Module};
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

fn with_temp_file<R>(tag: &str, ext: &str, f: impl FnOnce(&Path) -> R) -> R {
    let path = std::env::temp_dir().join(format!("{tag}.{ext}"));
    let result = f(&path);
    let _ = std::fs::remove_file(&path);
    result
}

fn run_ours(ctx: &Context, module: &Module, label: &str) -> Result<i32, String> {
    let main_func = module
        .functions
        .iter()
        .find(|f| f.name == "main" && !f.is_declaration)
        .ok_or_else(|| "missing non-declaration @main".to_string())?;

    let mut backend = X86Backend::default();
    let mut mf = backend.lower_function(ctx, module, main_func);
    let intervals = compute_live_intervals(&mf);
    let mut result = allocate_registers(
        &intervals,
        &mf.allocatable_pregs,
        RegAllocStrategy::LinearScan,
    );
    insert_spill_reloads(&mut mf, &mut result, MOV_LOAD_MR, MOV_STORE_RM);
    apply_allocation(&mut mf, &result);

    let obj_format = host_object_format().ok_or_else(|| "unsupported host OS".to_string())?;
    let mut emitter = X86Emitter::new(obj_format);
    let obj = emit_object(&mf, &mut emitter);
    let obj_bytes = obj.to_bytes();

    with_temp_file(&format!("run_ir_{label}"), "o", |obj_path| {
        std::fs::write(obj_path, &obj_bytes).map_err(|e| e.to_string())?;
        let bin_path = std::env::temp_dir().join(format!("run_ir_{label}_bin"));
        let link = Command::new("cc")
            .arg(obj_path)
            .arg("-o")
            .arg(&bin_path)
            .output()
            .map_err(|e| e.to_string())?;
        if !link.status.success() {
            return Err(format!(
                "link failed: {}",
                String::from_utf8_lossy(&link.stderr)
            ));
        }

        let run = Command::new(&bin_path)
            .output()
            .map_err(|e| e.to_string())?;
        let _ = std::fs::remove_file(&bin_path);
        Ok(run.status.code().unwrap_or(-1))
    })
}

fn main() {
    let mut args = std::env::args();
    let _exe = args.next();
    let ll_path = match args.next() {
        Some(p) => p,
        None => {
            eprintln!("usage: cargo run -p llvm-ir-parser --example run_ir -- <file.ll>");
            std::process::exit(2);
        }
    };

    let src = match std::fs::read_to_string(&ll_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("failed to read '{ll_path}': {e}");
            std::process::exit(1);
        }
    };

    let (mut ctx, mut module) = match parse(&src) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("parse failed: {e}");
            std::process::exit(1);
        }
    };

    let mut pm = PassManager::new();
    pm.add_function_pass(Mem2Reg);
    pm.add_function_pass(DeadCodeElim);
    pm.run(&mut ctx, &mut module);

    let label = Path::new(&ll_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("input");

    match run_ours(&ctx, &module, label) {
        Ok(exit_code) => println!("{exit_code}"),
        Err(e) => {
            eprintln!("pipeline failed: {e}");
            std::process::exit(1);
        }
    }
}
