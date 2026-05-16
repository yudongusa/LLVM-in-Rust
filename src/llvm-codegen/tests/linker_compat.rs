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

#[cfg(target_os = "macos")]
fn macho_obj_arch(obj_path: &Path) -> Option<String> {
    // Prefer lipo for a stable arch token (e.g. "arm64", "x86_64").
    if let Ok(out) = Command::new("lipo").arg("-archs").arg(obj_path).output() {
        if out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !s.is_empty() {
                return Some(s);
            }
        }
    }

    None
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

        let mut cmd = Command::new("cc");
        if let Some(arch) = macho_obj_arch(obj_path) {
            cmd.arg("-arch").arg(arch);
        }
        let link = cmd
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

#[cfg(target_os = "windows")]
#[test]
fn coff_link_with_lld_link_and_run_exit_code() {
    // Skip gracefully if lld-link is absent (e.g. bare Windows runner before
    // the LLVM choco package is installed).
    if !have_tool("lld-link") {
        eprintln!("lld-link not found; skipping coff_link_with_lld_link_and_run_exit_code");
        return;
    }

    with_temp_file("coff_e2e_main", "obj", |obj_path| {
        emit_host_obj(MAIN_RET42_LL, obj_path);

        let exe_path = std::env::temp_dir().join("coff_e2e_main.exe");
        // lld-link with no CRT: entry point is called directly by the Windows
        // loader (BaseProcessStart), which calls ExitProcess(main()) when main
        // returns, so the exit code equals the return value.
        let link = Command::new("lld-link")
            .arg(format!("/out:{}", exe_path.display()))
            .arg("/subsystem:console")
            .arg("/entry:main")
            .arg("/nodefaultlib")
            .arg("/machine:X64")
            .arg(obj_path)
            .output()
            .expect("spawn lld-link");
        assert!(
            link.status.success(),
            "lld-link failed:\n{}",
            String::from_utf8_lossy(&link.stderr)
        );

        let run = Command::new(&exe_path).output().expect("run exe");
        let _ = std::fs::remove_file(&exe_path);
        assert_eq!(run.status.code(), Some(42), "exit code must be 42");
    });
}

#[cfg(target_os = "windows")]
#[test]
fn coff_object_main_symbol_visible() {
    // Verify the COFF object file exposes `main` as a global symbol.
    // Uses llvm-nm when available, otherwise inspects raw bytes (the name
    // always appears verbatim in the COFF string table).
    with_temp_file("coff_sym_main", "obj", |obj_path| {
        emit_host_obj(MAIN_RET42_LL, obj_path);

        if have_tool("llvm-nm") {
            let nm = Command::new("llvm-nm")
                .arg(obj_path)
                .output()
                .expect("run llvm-nm");
            assert!(nm.status.success());
            let out = String::from_utf8_lossy(&nm.stdout);
            assert!(out.contains("main"), "llvm-nm output: {out}");
        } else {
            // Fallback: the symbol name appears verbatim in COFF string table.
            let bytes = std::fs::read(obj_path).expect("read obj");
            assert!(
                bytes.windows(4).any(|w| w == b"main"),
                "COFF bytes must contain 'main'"
            );
        }
    });
}

#[test]
fn tool_presence_report_is_accessible() {
    // Lightweight smoke to keep path used in CI logs if desired.
    let mut tools = Vec::<(&str, bool)>::new();
    for t in ["cc", "ld", "lld", "lld-link", "readelf", "nm", "llvm-nm", "objdump", "otool"] {
        tools.push((t, have_tool(t)));
    }
    assert!(!tools.is_empty());
}
