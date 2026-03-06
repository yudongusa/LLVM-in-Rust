use std::path::PathBuf;
use std::process::Command;

use llvm_codegen::{
    emit_object,
    isel::IselBackend,
    regalloc::{allocate_registers, apply_allocation, compute_live_intervals, RegAllocStrategy},
    ObjectFormat,
};
use llvm_ir_parser::parser::parse;
use llvm_target_riscv::{RiscVBackend, RiscVEmitter};
use llvm_transforms::{pass::PassManager, DeadCodeElim, Mem2Reg};

const SAMPLE_LL: &str = include_str!("../../llvm-bench/fixtures/sample.ll");

fn find_objdump() -> Option<PathBuf> {
    [
        "riscv64-linux-gnu-objdump",
        "llvm-objdump",
        "objdump",
    ]
    .iter()
    .find_map(|name| {
        Command::new(name)
            .arg("--version")
            .output()
            .ok()
            .map(|_| PathBuf::from(name))
    })
}

#[test]
fn sample_ll_emits_riscv_elf_objdump_accepts() {
    let Some(objdump) = find_objdump() else {
        return;
    };

    let (mut ctx, mut module) = parse(SAMPLE_LL).expect("sample.ll parse");

    // Reduce memory ops/phi pressure to a form the backend handles robustly.
    let mut pm = PassManager::new();
    pm.add_function_pass(Mem2Reg);
    pm.add_function_pass(DeadCodeElim);
    pm.run(&mut ctx, &mut module);

    let mut backend = RiscVBackend::default();
    for func in &module.functions {
        if func.is_declaration {
            continue;
        }
        let mut mf = backend.lower_function(&ctx, &module, func);
        let intervals = compute_live_intervals(&mf);
        let result = allocate_registers(
            &intervals,
            &mf.allocatable_pregs,
            RegAllocStrategy::LinearScan,
        );
        apply_allocation(&mut mf, &result);

        let mut emitter = RiscVEmitter::new(ObjectFormat::Elf);
        let obj = emit_object(&mf, &mut emitter);
        let bytes = obj.to_bytes();
        assert_eq!(&bytes[0..4], b"\x7fELF");
        let e_machine = u16::from_le_bytes([bytes[18], bytes[19]]);
        assert_eq!(e_machine, 243, "EM_RISCV");

        let tmp = std::env::temp_dir().join(format!("{}_riscv.o", func.name));
        std::fs::write(&tmp, &bytes).expect("write obj");

        let out = Command::new(&objdump)
            .arg("-f")
            .arg(&tmp)
            .output()
            .expect("run objdump");
        let _ = std::fs::remove_file(&tmp);
        assert!(
            out.status.success(),
            "objdump failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        let stdout = String::from_utf8_lossy(&out.stdout);
        if objdump.file_name().and_then(|s| s.to_str()) == Some("llvm-objdump") {
            assert!(
                stdout.to_lowercase().contains("elf64") || stdout.contains("EM_RISCV"),
                "unexpected llvm-objdump output: {stdout}"
            );
        } else {
            assert!(
                stdout.contains("elf64-littleriscv")
                    || stdout.contains("elf64-little")
                    || stdout.contains("riscv"),
                "unexpected objdump output: {stdout}"
            );
        }
    }
}
