use std::path::Path;
use std::process::Command;

use llvm_codegen::{
    emit_object,
    isel::IselBackend,
    regalloc::{allocate_registers, apply_allocation, compute_live_intervals, RegAllocStrategy},
    ObjectFormat,
};
use llvm_ir_parser::parser::parse;

const MAIN_RET42_LL: &str = r#"
define i32 @main() {
entry:
  ret i32 42
}
"#;

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

fn emit_host_obj(ll: &str, out: &Path) {
    let (ctx, module) = parse(ll).expect("parse test ir");
    let func = module
        .functions
        .iter()
        .find(|f| f.name == "main" && !f.is_declaration)
        .expect("@main must exist");

    let obj_format = host_object_format().expect("unsupported host object format");

    #[cfg(target_arch = "x86_64")]
    {
        emit_host_obj_x86(&ctx, &module, func, obj_format, out);
        return;
    }

    #[cfg(target_arch = "aarch64")]
    {
        emit_host_obj_aarch64(&ctx, &module, func, obj_format, out);
        return;
    }

    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    panic!("unsupported host arch for linker_compat test");
}

#[cfg(target_arch = "x86_64")]
fn emit_host_obj_x86(
    ctx: &llvm_ir::Context,
    module: &llvm_ir::Module,
    func: &llvm_ir::Function,
    obj_format: ObjectFormat,
    out: &Path,
) {
    use llvm_target_x86::{
        instructions::{MOV_LOAD_MR, MOV_STORE_RM},
        X86Backend, X86Emitter,
    };

    let mut backend = X86Backend::default();
    let mut mf = backend.lower_function(ctx, module, func);
    let intervals = compute_live_intervals(&mf);
    let mut result = allocate_registers(
        &intervals,
        &mf.allocatable_pregs,
        RegAllocStrategy::LinearScan,
    );
    llvm_codegen::regalloc::insert_spill_reloads(&mut mf, &mut result, MOV_LOAD_MR, MOV_STORE_RM);
    apply_allocation(&mut mf, &result);
    let mut emitter = X86Emitter::new(obj_format);
    let obj = emit_object(&mf, &mut emitter);
    std::fs::write(out, obj.to_bytes()).expect("write object");
}

#[cfg(target_arch = "aarch64")]
fn emit_host_obj_aarch64(
    ctx: &llvm_ir::Context,
    module: &llvm_ir::Module,
    func: &llvm_ir::Function,
    obj_format: ObjectFormat,
    out: &Path,
) {
    use llvm_target_arm::{
        encode::AArch64Emitter,
        instructions::{LDR_FP, STR_FP},
        lower::AArch64Backend,
    };

    let mut backend = AArch64Backend;
    let mut mf = backend.lower_function(ctx, module, func);
    let intervals = compute_live_intervals(&mf);
    let mut result = allocate_registers(
        &intervals,
        &mf.allocatable_pregs,
        RegAllocStrategy::LinearScan,
    );
    llvm_codegen::regalloc::insert_spill_reloads(&mut mf, &mut result, LDR_FP, STR_FP);
    apply_allocation(&mut mf, &result);
    let mut emitter = AArch64Emitter::new(obj_format);
    let obj = emit_object(&mf, &mut emitter);
    std::fs::write(out, obj.to_bytes()).expect("write object");
}

fn with_temp_file<R>(tag: &str, ext: &str, f: impl FnOnce(&Path) -> R) -> R {
    let path = std::env::temp_dir().join(format!("{tag}.{ext}"));
    let result = f(&path);
    let _ = std::fs::remove_file(&path);
    result
}

#[cfg(target_os = "linux")]
#[test]
fn elf_link_with_cc_and_run_exit_code() {
    if !have_tool("cc") {
        return;
    }

    with_temp_file("linker_compat_main", "o", |obj_path| {
        emit_host_obj(MAIN_RET42_LL, obj_path);

        let bin_path = std::env::temp_dir().join("linker_compat_main_bin");
        let link = Command::new("cc")
            .arg(obj_path)
            .arg("-o")
            .arg(&bin_path)
            .output()
            .expect("run cc");
        assert!(
            link.status.success(),
            "cc link failed: {}",
            String::from_utf8_lossy(&link.stderr)
        );

        let run = Command::new(&bin_path).output().expect("run binary");
        let _ = std::fs::remove_file(&bin_path);
        assert_eq!(run.status.code(), Some(42));
    });
}

#[cfg(target_os = "linux")]
#[test]
fn elf_readelf_and_nm_show_expected_entries() {
    if !have_tool("readelf") || !have_tool("nm") {
        return;
    }

    with_temp_file("linker_compat_info", "o", |obj_path| {
        emit_host_obj(MAIN_RET42_LL, obj_path);

        let readelf = Command::new("readelf")
            .arg("-a")
            .arg(obj_path)
            .output()
            .expect("run readelf");
        assert!(readelf.status.success());
        let re = String::from_utf8_lossy(&readelf.stdout);
        assert!(re.contains(".text"));
        assert!(re.contains("Symbol table"));

        let nm = Command::new("nm").arg(obj_path).output().expect("run nm");
        assert!(nm.status.success());
        let nm_out = String::from_utf8_lossy(&nm.stdout);
        assert!(nm_out.contains(" main"), "nm output: {nm_out}");
    });
}

#[cfg(target_os = "macos")]
#[test]
fn macho_link_with_cc_succeeds() {
    if !have_tool("cc") {
        return;
    }

    with_temp_file("linker_compat_main", "o", |obj_path| {
        emit_host_obj(MAIN_RET42_LL, obj_path);
        let bin_path = std::env::temp_dir().join("linker_compat_main_bin");
        let link = Command::new("cc")
            .arg(obj_path)
            .arg("-o")
            .arg(&bin_path)
            .output()
            .expect("run cc");
        assert!(
            link.status.success(),
            "cc link failed: {}",
            String::from_utf8_lossy(&link.stderr)
        );
        let _ = std::fs::remove_file(&bin_path);
    });
}

#[cfg(target_os = "macos")]
#[test]
fn macho_nm_lists_main() {
    if !have_tool("nm") {
        return;
    }

    with_temp_file("linker_compat_nm", "o", |obj_path| {
        emit_host_obj(MAIN_RET42_LL, obj_path);
        let nm = Command::new("nm").arg(obj_path).output().expect("run nm");
        assert!(nm.status.success());
        let out = String::from_utf8_lossy(&nm.stdout);
        assert!(
            out.contains(" _main") || out.contains(" main"),
            "nm output: {out}"
        );
    });
}

#[test]
fn tool_presence_report_is_accessible() {
    // Lightweight smoke to keep path used in CI logs if desired.
    let mut tools = Vec::<(&str, bool)>::new();
    for t in ["cc", "ld", "lld", "readelf", "nm", "objdump", "otool"] {
        tools.push((t, have_tool(t)));
    }
    assert!(!tools.is_empty());
}
