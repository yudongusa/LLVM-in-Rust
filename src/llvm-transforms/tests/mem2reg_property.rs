use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use llvm_codegen::{
    emit_object,
    isel::IselBackend,
    regalloc::{
        allocate_registers, apply_allocation, compute_live_intervals, insert_spill_reloads,
        RegAllocStrategy,
    },
    ObjectFormat,
};
use llvm_ir::printer::Printer;
use llvm_ir_parser::parser::parse;
use llvm_target_x86::{
    instructions::{MOV_LOAD_MR, MOV_STORE_RM},
    X86Backend, X86Emitter,
};
use llvm_transforms::{pass::FunctionPass, Mem2Reg};
use proptest::prelude::*;

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

fn have_tool(name: &str) -> bool {
    Command::new(name).arg("--version").output().is_ok()
}

fn host_object_format() -> Option<ObjectFormat> {
    if cfg!(target_os = "linux") {
        Some(ObjectFormat::Elf)
    } else if cfg!(target_os = "windows") {
        Some(ObjectFormat::Coff)
    } else if cfg!(target_os = "macos") {
        Some(ObjectFormat::MachO)
    } else {
        None
    }
}

fn unique_tmp(prefix: &str, ext: &str) -> PathBuf {
    let n = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("{prefix}_{}_{}.{}", std::process::id(), n, ext))
}

fn build_program(init: i32, ops: &[(u8, i32)]) -> String {
    let mut out = String::new();
    out.push_str("define i32 @main() {\nentry:\n");
    out.push_str("  %p = alloca i32\n");
    out.push_str(&format!("  store i32 {init}, ptr %p\n"));

    for (i, (op, k)) in ops.iter().copied().enumerate() {
        match op % 4 {
            0 => {
                out.push_str(&format!("  store i32 {k}, ptr %p\n"));
            }
            1 => {
                out.push_str(&format!("  %l{i} = load i32, ptr %p\n"));
                out.push_str(&format!("  %a{i} = add i32 %l{i}, {k}\n"));
                out.push_str(&format!("  store i32 %a{i}, ptr %p\n"));
            }
            2 => {
                out.push_str(&format!("  %l{i} = load i32, ptr %p\n"));
                out.push_str(&format!("  %s{i} = sub i32 %l{i}, {k}\n"));
                out.push_str(&format!("  store i32 %s{i}, ptr %p\n"));
            }
            _ => {
                out.push_str(&format!("  %l{i} = load i32, ptr %p\n"));
                out.push_str(&format!("  %x{i} = xor i32 %l{i}, {k}\n"));
                out.push_str(&format!("  store i32 %x{i}, ptr %p\n"));
            }
        }
    }

    out.push_str("  %retv = load i32, ptr %p\n");
    out.push_str("  ret i32 %retv\n}\n");
    out
}

fn mem2reg_transform(src: &str) -> Option<String> {
    let (mut ctx, mut module) = parse(src).ok()?;
    let func = module.functions.iter_mut().find(|f| f.name == "main")?;
    let mut pass = Mem2Reg;
    let _ = pass.run_on_function(&mut ctx, func);
    Some(Printer::new(&ctx).print_module(&module))
}

fn compile_and_run_clang_exit_code(src: &str) -> Option<i32> {
    let ll_path = unique_tmp("mem2reg_prop", "ll");
    let bin_path = unique_tmp("mem2reg_prop", "bin");
    std::fs::write(&ll_path, src).ok()?;
    let compile = Command::new("clang")
        .args(["-x", "ir", "-O0"])
        .arg(&ll_path)
        .arg("-o")
        .arg(&bin_path)
        .output()
        .ok()?;
    if !compile.status.success() {
        let _ = std::fs::remove_file(&ll_path);
        let _ = std::fs::remove_file(&bin_path);
        return None;
    }
    let run = Command::new(&bin_path).output().ok()?;
    let _ = std::fs::remove_file(&ll_path);
    let _ = std::fs::remove_file(&bin_path);
    Some(run.status.code().unwrap_or(-1))
}

fn compile_and_run_ours_exit_code(src: &str) -> Option<i32> {
    let (ctx, module) = parse(src).ok()?;

    let obj_format = host_object_format()?;
    let mut backend = X86Backend::default();
    let main_func = module
        .functions
        .iter()
        .find(|f| f.name == "main" && !f.is_declaration)?;

    let mut mf = backend.lower_function(&ctx, &module, main_func);
    let intervals = compute_live_intervals(&mf);
    let mut result = allocate_registers(
        &intervals,
        &mf.allocatable_pregs,
        RegAllocStrategy::LinearScan,
    );
    insert_spill_reloads(&mut mf, &mut result, MOV_LOAD_MR, MOV_STORE_RM);
    apply_allocation(&mut mf, &result);

    let mut emitter = X86Emitter::new(obj_format);
    let obj = emit_object(&mf, &mut emitter);

    let obj_path = unique_tmp("mem2reg_prop", "o");
    let bin_path = unique_tmp("mem2reg_prop", "bin");
    std::fs::write(&obj_path, obj.to_bytes()).ok()?;

    let link = Command::new("cc")
        .arg(&obj_path)
        .arg("-o")
        .arg(&bin_path)
        .output()
        .ok()?;
    if !link.status.success() {
        let _ = std::fs::remove_file(&obj_path);
        let _ = std::fs::remove_file(&bin_path);
        return None;
    }

    let run = Command::new(&bin_path).output().ok()?;
    let _ = std::fs::remove_file(&obj_path);
    let _ = std::fs::remove_file(&bin_path);
    Some(run.status.code().unwrap_or(-1))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn mem2reg_semantics_equivalent_random_patterns(
        init in -128i32..128i32,
        ops in prop::collection::vec((0u8..4u8, -128i32..128i32), 1..24)
    ) {
        if !have_tool("clang") {
            return Ok(());
        }

        let src = build_program(init, &ops);
        let after_src = mem2reg_transform(&src)
            .ok_or_else(|| TestCaseError::fail("failed to run mem2reg transform"))?;

        let before = compile_and_run_clang_exit_code(&src)
            .ok_or_else(|| TestCaseError::fail("failed to run original program with clang"))?;
        let after = compile_and_run_clang_exit_code(&after_src)
            .ok_or_else(|| TestCaseError::fail("failed to run mem2reg program with clang"))?;

        prop_assert_eq!(before, after, "program:\n{}", src);

        if have_tool("cc") && cfg!(target_arch = "x86_64") {
            if let Some(ours_after) = compile_and_run_ours_exit_code(&after_src) {
                prop_assert_eq!(ours_after, after, "transformed program:\n{}", after_src);
            }
        }
    }
}
