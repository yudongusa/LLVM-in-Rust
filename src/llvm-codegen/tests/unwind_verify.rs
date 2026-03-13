use std::path::{Path, PathBuf};
use std::process::Command;

use llvm_codegen::{emit_object, isel::MachineFunction, ObjectFormat};
use llvm_target_x86::{regs::RBX, X86Emitter};

fn find_tool(candidates: &[&str]) -> Option<PathBuf> {
    for cand in candidates {
        let path = PathBuf::from(cand);
        let probe = if path.components().count() > 1 {
            Command::new(&path).arg("--version").output()
        } else {
            Command::new(cand).arg("--version").output()
        };
        if probe.is_ok() {
            return Some(path);
        }
    }
    None
}

fn require_tool(candidates: &[&str], display_name: &str) -> Option<PathBuf> {
    if let Some(path) = find_tool(candidates) {
        return Some(path);
    }
    if std::env::var("REQUIRE_LLVM").is_ok() {
        panic!(
            "REQUIRE_LLVM is set but '{}' was not found on PATH.",
            display_name
        );
    }
    None
}

fn emit_obj(format: ObjectFormat, out: &Path) {
    let mut mf = MachineFunction::new("u".into());
    mf.add_block("entry");
    mf.frame_size = 16;
    mf.used_callee_saved = vec![RBX];

    let mut emitter = X86Emitter::new(format);
    let obj = emit_object(&mf, &mut emitter);
    std::fs::write(out, obj.to_bytes()).expect("write object file");
}

#[test]
fn elf_eh_frame_verifies_with_readelf() {
    let obj_path = std::env::temp_dir().join("llvm_codegen_unwind_elf.o");
    emit_obj(ObjectFormat::Elf, &obj_path);

    if let Some(readelf) = require_tool(&["readelf"], "readelf") {
        let out = Command::new(&readelf)
            .arg("--debug-dump=frames")
            .arg(&obj_path)
            .output()
            .expect("run readelf --debug-dump=frames");
        assert!(out.status.success());
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            text.contains("CIE") || text.contains("FDE"),
            "unexpected readelf frames output: {text}"
        );
    }

    if let Some(dwarfdump) = require_tool(
        &[
            "llvm-dwarfdump",
            "llvm-dwarfdump-19",
            "/usr/lib/llvm-19/bin/llvm-dwarfdump",
        ],
        "llvm-dwarfdump",
    ) {
        let out = Command::new(&dwarfdump)
            .arg("--eh-frame")
            .arg(&obj_path)
            .output()
            .expect("run llvm-dwarfdump --eh-frame");
        assert!(out.status.success());
    }

    let _ = std::fs::remove_file(&obj_path);
}

#[test]
fn coff_unwind_tables_visible_to_llvm_readobj() {
    let obj_path = std::env::temp_dir().join("llvm_codegen_unwind_coff.obj");
    emit_obj(ObjectFormat::Coff, &obj_path);

    if let Some(readobj) = require_tool(
        &["llvm-readobj", "llvm-readobj-19", "/usr/lib/llvm-19/bin/llvm-readobj"],
        "llvm-readobj",
    ) {
        let out = Command::new(&readobj)
            .arg("--unwind")
            .arg(&obj_path)
            .output()
            .expect("run llvm-readobj --unwind");
        assert!(out.status.success());
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            text.contains("UnwindInformation")
                || text.contains("RuntimeFunction")
                || text.contains("UnwindData"),
            "unexpected llvm-readobj --unwind output: {text}"
        );
    }

    let _ = std::fs::remove_file(&obj_path);
}
