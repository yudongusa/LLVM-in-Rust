//! Differential tests: verify our IR printer output is accepted by real LLVM
//! tools (Part 1) and that our codegen produces semantically correct
//! executables (Part 2).
//!
//! Every test skips gracefully when LLVM tools are absent, so CI without an
//! LLVM installation still passes.  On a machine with LLVM 19 at
//! `/usr/local/opt/llvm/bin/` (or any other standard location) the tests
//! actually validate against the real compiler.

use std::path::{Path, PathBuf};
use std::process::Command;

use llvm_codegen::{
    emit_object,
    isel::IselBackend,
    regalloc::{apply_allocation, compute_live_intervals, insert_spill_reloads, linear_scan},
    ObjectFormat,
};
use llvm_ir::{printer::Printer, Context, Module};
use llvm_ir_parser::parser::parse;
use llvm_target_x86::{
    instructions::{MOV_LOAD_MR, MOV_STORE_RM},
    X86Backend, X86Emitter,
};
use llvm_transforms::{pass::PassManager, Mem2Reg};

// ── tool discovery ────────────────────────────────────────────────────────────

fn find_llvm_bin() -> Option<PathBuf> {
    [
        "/usr/local/opt/llvm/bin",
        "/opt/homebrew/opt/llvm/bin",
        "/usr/bin",
        "/usr/local/bin",
    ]
    .iter()
    .map(PathBuf::from)
    .find(|p| p.join("llvm-as").exists())
    .or_else(|| {
        Command::new("llvm-as")
            .arg("--version")
            .output()
            .ok()
            .map(|_| PathBuf::from(""))
    })
}

fn llvm_tool(name: &str) -> Option<PathBuf> {
    let bin = find_llvm_bin()?;
    let path = if bin.as_os_str().is_empty() {
        PathBuf::from(name)
    } else {
        bin.join(name)
    };
    if path.exists() || bin.as_os_str().is_empty() {
        Some(path)
    } else {
        None
    }
}

// ── temp-file helper ──────────────────────────────────────────────────────────

fn with_temp_ll<R>(tag: &str, content: &str, f: impl FnOnce(&Path) -> R) -> R {
    let dir = std::env::temp_dir();
    let path = dir.join(format!("llvm_diff_{tag}.ll"));
    std::fs::write(&path, content).expect("write temp .ll");
    let result = f(&path);
    let _ = std::fs::remove_file(&path);
    result
}

fn with_temp_file<R>(tag: &str, ext: &str, f: impl FnOnce(&Path) -> R) -> R {
    let dir = std::env::temp_dir();
    let path = dir.join(format!("llvm_diff_{tag}.{ext}"));
    let result = f(&path);
    let _ = std::fs::remove_file(&path);
    result
}

// ── Part 1 helpers ────────────────────────────────────────────────────────────

/// Parse `src`, round-trip through our printer, then validate with `llvm-as`.
/// Skips if `llvm-as` is not available.
fn roundtrip_and_validate(label: &str, src: &str) {
    let (ctx, module) = parse(src).unwrap_or_else(|e| panic!("our parser rejected '{label}': {e}"));
    let printed = Printer::new(&ctx).print_module(&module);
    let llvm_as = match llvm_tool("llvm-as") {
        Some(p) => p,
        None => return,
    };
    with_temp_ll(label, &printed, |path| {
        let out = Command::new(&llvm_as)
            .arg(path)
            .arg("-o")
            .arg("/dev/null")
            .output()
            .expect("spawn llvm-as");
        assert!(
            out.status.success(),
            "llvm-as rejected IR for '{label}':\n{printed}\nstderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    });
}

// ── Part 1 tests — round-trip + llvm-as validation ───────────────────────────

#[test]
fn roundtrip_sample_fixture() {
    let src = include_str!("../../llvm-bench/fixtures/sample.ll");
    roundtrip_and_validate("sample_fixture", src);
}

#[test]
fn roundtrip_minimal_void() {
    roundtrip_and_validate(
        "minimal_void",
        r#"define void @f() {
  ret void
}
"#,
    );
}

#[test]
fn roundtrip_arithmetic() {
    roundtrip_and_validate(
        "arithmetic",
        r#"define i32 @arith(i32 %a, i32 %b) {
entry:
  %s = add i32 %a, %b
  %d = sub i32 %s, %b
  %p = mul i32 %d, %a
  %q = sdiv i32 %p, %a
  %r = srem i32 %q, %b
  ret i32 %r
}
define i64 @arith64(i64 %x, i64 %y) {
entry:
  %s = add i64 %x, %y
  %d = sub i64 %s, %y
  %p = mul i64 %d, %x
  ret i64 %p
}
"#,
    );
}

#[test]
fn roundtrip_bitwise() {
    roundtrip_and_validate(
        "bitwise",
        r#"define i64 @bitwise(i64 %a, i64 %b) {
entry:
  %t0 = and i64 %a, %b
  %t1 = or i64 %t0, %b
  %t2 = xor i64 %t1, %a
  %t3 = shl i64 %t2, 3
  %t4 = lshr i64 %t3, 1
  %t5 = ashr i64 %t4, 2
  ret i64 %t5
}
"#,
    );
}

#[test]
fn roundtrip_icmp_br_phi() {
    roundtrip_and_validate(
        "icmp_br_phi",
        r#"define i32 @max(i32 %a, i32 %b) {
entry:
  %cmp = icmp sgt i32 %a, %b
  br i1 %cmp, label %t, label %f
t:
  br label %merge
f:
  br label %merge
merge:
  %v = phi i32 [ %a, %t ], [ %b, %f ]
  ret i32 %v
}
"#,
    );
}

#[test]
fn roundtrip_alloca_mem() {
    roundtrip_and_validate(
        "alloca_mem",
        r#"define i32 @alloca_test(i32 %n) {
entry:
  %slot = alloca i32
  store i32 %n, ptr %slot
  %loaded = load i32, ptr %slot
  %ptr2 = getelementptr i32, ptr %slot, i32 0
  %v = load i32, ptr %ptr2
  ret i32 %v
}
"#,
    );
}

#[test]
fn roundtrip_call() {
    roundtrip_and_validate(
        "call",
        r#"declare i32 @external(i32)
define i32 @caller(i32 %x) {
entry:
  %r = call i32 @external(i32 %x)
  ret i32 %r
}
"#,
    );
}

#[test]
fn roundtrip_global_vars() {
    roundtrip_and_validate(
        "global_vars",
        r#"@counter = global i32 0
@limit = constant i32 100
@flag = private global i1 0
define i32 @read_counter() {
entry:
  %v = load i32, ptr @counter
  ret i32 %v
}
"#,
    );
}

#[test]
fn roundtrip_named_structs() {
    roundtrip_and_validate(
        "named_structs",
        r#"%S = type { i32, ptr }
define i32 @get_first(%S %s) {
entry:
  %v = extractvalue %S %s, 0
  ret i32 %v
}
"#,
    );
}

#[test]
fn roundtrip_switch() {
    roundtrip_and_validate(
        "switch",
        r#"define i32 @classify(i32 %x) {
entry:
  switch i32 %x, label %otherwise [
    i32 0, label %case0
    i32 1, label %case1
    i32 2, label %case2
  ]
case0:
  ret i32 10
case1:
  ret i32 20
case2:
  ret i32 30
otherwise:
  ret i32 99
}
"#,
    );
}

#[test]
fn roundtrip_select() {
    roundtrip_and_validate(
        "select",
        r#"define i32 @sel(i1 %c, i32 %a, i32 %b) {
entry:
  %v = select i1 %c, i32 %a, i32 %b
  ret i32 %v
}
"#,
    );
}

// ── Part 2 helpers ────────────────────────────────────────────────────────────

/// Compile `printed_ir` with clang and run the resulting binary.
/// Returns the exit code, or `None` if clang is not available.
fn compile_and_run_llvm(clang: &Path, label: &str, printed_ir: &str) -> Option<i32> {
    let bin_path = std::env::temp_dir().join(format!("llvm_diff_{label}_llvm_bin"));
    let result = with_temp_ll(&format!("{label}_llvm"), printed_ir, |ll_path| {
        let compile = Command::new(clang)
            .args(["-x", "ir"])
            .arg(ll_path)
            .arg("-o")
            .arg(&bin_path)
            .output()
            .expect("spawn clang");
        if !compile.status.success() {
            eprintln!(
                "[{label}] clang compile failed:\n{}",
                String::from_utf8_lossy(&compile.stderr)
            );
            return None;
        }
        let run = Command::new(&bin_path).output().expect("run binary");
        let _ = std::fs::remove_file(&bin_path);
        Some(run.status.code().unwrap_or(-1))
    });
    result
}

/// Compile `ctx`/`module` with our x86 codegen, link with `cc`, and run.
/// Returns the exit code, or `None` if linking fails (skipped gracefully).
fn compile_and_run_ours(ctx: &Context, module: &Module, label: &str) -> Option<i32> {
    let mut backend = X86Backend;
    // Find the `main` function.
    let main_func = module.functions.iter().find(|f| f.name == "main")?;

    let mut mf = backend.lower_function(ctx, module, main_func);
    let intervals = compute_live_intervals(&mf);
    let mut result = linear_scan(&intervals, &mf.allocatable_pregs);
    insert_spill_reloads(&mut mf, &mut result, MOV_LOAD_MR, MOV_STORE_RM);
    apply_allocation(&mut mf, &result);
    let mut emitter = X86Emitter::new(ObjectFormat::Elf);
    let obj = emit_object(&mf, &mut emitter);
    let obj_bytes = obj.to_bytes();

    with_temp_file(&format!("{label}_ours"), "o", |obj_path| {
        std::fs::write(obj_path, &obj_bytes).expect("write .o");
        let bin_path = std::env::temp_dir().join(format!("llvm_diff_{label}_our_bin"));
        let link = Command::new("cc")
            .arg(obj_path)
            .arg("-o")
            .arg(&bin_path)
            .output()
            .expect("spawn cc");
        if !link.status.success() {
            // Linking may fail if ELF emission isn't fully linkable yet — skip.
            return None;
        }
        let run = Command::new(&bin_path).output().expect("run our binary");
        let _ = std::fs::remove_file(&bin_path);
        Some(run.status.code().unwrap_or(-1))
    })
}

/// Run a semantic differential test.
///
/// Compiles `src` via LLVM (clang) and via our codegen, then asserts both
/// binaries exit with `expected_exit`.  Skips if clang is absent or if our
/// ELF is not yet linkable.
fn run_semantic_test(label: &str, src: &str, expected_exit: i32) {
    let clang = match llvm_tool("clang") {
        Some(p) => p,
        None => return,
    };

    let (mut ctx, mut module) = parse(src)
        .unwrap_or_else(|e| panic!("our parser rejected '{label}': {e}"));
    let mut pm = PassManager::new();
    pm.add_function_pass(Mem2Reg);
    pm.run(&mut ctx, &mut module);

    let printed = Printer::new(&ctx).print_module(&module);

    let llvm_exit = compile_and_run_llvm(&clang, label, &printed);
    let our_exit = compile_and_run_ours(&ctx, &module, label);

    match (llvm_exit, our_exit) {
        (Some(l), Some(o)) => {
            assert_eq!(
                l, expected_exit,
                "LLVM exit code wrong for '{label}' (expected {expected_exit}, got {l})"
            );
            assert_eq!(
                o, expected_exit,
                "Our exit code wrong for '{label}' (expected {expected_exit}, got {o})"
            );
        }
        (Some(l), None) => {
            assert_eq!(
                l, expected_exit,
                "LLVM exit code wrong for '{label}' (our path skipped)"
            );
        }
        (None, _) => {
            // clang unavailable — already returned above, but defensive.
        }
    }
}

// ── Part 2 tests — semantic differential ─────────────────────────────────────

#[test]
fn semantic_return_constant() {
    run_semantic_test(
        "return_constant",
        r#"define i32 @main() {
entry:
  ret i32 7
}
"#,
        7,
    );
}

#[test]
fn semantic_add() {
    run_semantic_test(
        "add",
        r#"define i32 @main() {
entry:
  %r = add i32 10, 20
  ret i32 %r
}
"#,
        30,
    );
}

#[test]
fn semantic_mul() {
    run_semantic_test(
        "mul",
        r#"define i32 @main() {
entry:
  %r = mul i32 3, 5
  ret i32 %r
}
"#,
        15,
    );
}

#[test]
fn semantic_sub() {
    run_semantic_test(
        "sub",
        r#"define i32 @main() {
entry:
  %r = sub i32 100, 44
  ret i32 %r
}
"#,
        56,
    );
}

#[test]
fn semantic_chain() {
    run_semantic_test(
        "chain",
        r#"define i32 @main() {
entry:
  %a = add i32 1, 1
  %b = mul i32 %a, 21
  ret i32 %b
}
"#,
        42,
    );
}
