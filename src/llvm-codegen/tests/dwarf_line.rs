use std::path::Path;
use std::process::Command;

use llvm_codegen::{
    emit_object,
    isel::IselBackend,
    regalloc::{allocate_registers, apply_allocation, compute_live_intervals, RegAllocStrategy},
    ObjectFormat,
};
use llvm_ir_parser::parser::parse;

const DBG_LL: &str = r#"
source_filename = "dbg_test.c"
define i32 @main() {
entry:
  ret i32 0, !dbg !12
}
!12 = !DILocation(line: 42, column: 7, scope: !1)
"#;

fn have_tool(name: &str) -> bool {
    Command::new(name).arg("--version").output().is_ok()
}

#[cfg(target_arch = "x86_64")]
fn emit_dbg_elf_obj(out: &Path) {
    use llvm_target_x86::{
        instructions::{MOV_LOAD_MR, MOV_STORE_RM},
        X86Backend, X86Emitter,
    };

    let (ctx, module) = parse(DBG_LL).expect("parse test ir");
    let func = module
        .functions
        .iter()
        .find(|f| f.name == "main" && !f.is_declaration)
        .expect("@main must exist");

    let mut backend = X86Backend::default();
    let mut mf = backend.lower_function(&ctx, &module, func);
    let intervals = compute_live_intervals(&mf);
    let mut result = allocate_registers(
        &intervals,
        &mf.allocatable_pregs,
        RegAllocStrategy::LinearScan,
    );
    llvm_codegen::regalloc::insert_spill_reloads(&mut mf, &mut result, MOV_LOAD_MR, MOV_STORE_RM);
    apply_allocation(&mut mf, &result);
    let mut emitter = X86Emitter::new(ObjectFormat::Elf);
    let obj = emit_object(&mf, &mut emitter);

    assert!(obj.sections.iter().any(|s| s.name == ".debug_line"));
    std::fs::write(out, obj.to_bytes()).expect("write object");
}

#[cfg(target_arch = "aarch64")]
fn emit_dbg_elf_obj(out: &Path) {
    use llvm_target_arm::{
        encode::AArch64Emitter,
        instructions::{LDR_FP, STR_FP},
        lower::AArch64Backend,
    };

    let (ctx, module) = parse(DBG_LL).expect("parse test ir");
    let func = module
        .functions
        .iter()
        .find(|f| f.name == "main" && !f.is_declaration)
        .expect("@main must exist");

    let mut backend = AArch64Backend;
    let mut mf = backend.lower_function(&ctx, &module, func);
    let intervals = compute_live_intervals(&mf);
    let mut result = allocate_registers(
        &intervals,
        &mf.allocatable_pregs,
        RegAllocStrategy::LinearScan,
    );
    llvm_codegen::regalloc::insert_spill_reloads(&mut mf, &mut result, LDR_FP, STR_FP);
    apply_allocation(&mut mf, &result);
    let mut emitter = AArch64Emitter::new(ObjectFormat::Elf);
    let obj = emit_object(&mf, &mut emitter);

    assert!(obj.sections.iter().any(|s| s.name == ".debug_line"));
    std::fs::write(out, obj.to_bytes()).expect("write object");
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
fn emit_dbg_elf_obj(_out: &Path) {
    panic!("unsupported host arch for dwarf_line test");
}

#[test]
fn emits_debug_line_when_dbg_metadata_present() {
    let obj_path = std::env::temp_dir().join("llvm_codegen_dbg_line.o");
    emit_dbg_elf_obj(&obj_path);

    if have_tool("readelf") {
        let out = Command::new("readelf")
            .arg("-S")
            .arg(&obj_path)
            .output()
            .expect("run readelf");
        assert!(out.status.success());
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains(".debug_line"), "readelf output: {text}");
    }

    if have_tool("llvm-dwarfdump") {
        let out = Command::new("llvm-dwarfdump")
            .arg("--debug-line")
            .arg(&obj_path)
            .output()
            .expect("run llvm-dwarfdump");
        assert!(out.status.success());
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains("dbg_test.c") || text.contains("line"), "dwarfdump output: {text}");
    }

    let _ = std::fs::remove_file(&obj_path);
}
