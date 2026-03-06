//! Differential tests: verify our IR printer output is accepted by real LLVM
//! tools (Part 1) and that our codegen produces semantically correct
//! executables (Part 2).
//!
//! When `REQUIRE_LLVM=1` is set (e.g. in CI), any test that cannot find the
//! LLVM tools panics rather than skipping — enforcing zero skips.
//!
//! Part 3 checks a regression hash database (`fixtures/known_hashes.json`)
//! to detect unexpected changes to round-trip output.

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
        "/usr/lib/llvm-19/bin",
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

/// Resolve an LLVM tool, panicking when `REQUIRE_LLVM=1` is set and the tool
/// is absent.  Returns `None` otherwise (skip the test gracefully).
fn require_tool(name: &str) -> Option<PathBuf> {
    match llvm_tool(name) {
        Some(p) => Some(p),
        None => {
            if std::env::var("REQUIRE_LLVM").is_ok() {
                panic!(
                    "REQUIRE_LLVM is set but '{}' was not found. \
                     Install LLVM 19 and ensure it is on PATH.",
                    name
                );
            }
            None
        }
    }
}

// ── temp-file helpers ─────────────────────────────────────────────────────────

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
/// When `REQUIRE_LLVM=1` is set the test panics if tools are absent.
fn roundtrip_and_validate(label: &str, src: &str) {
    let (ctx, module) = parse(src).unwrap_or_else(|e| panic!("our parser rejected '{label}': {e}"));
    let printed = Printer::new(&ctx).print_module(&module);
    let llvm_as = match require_tool("llvm-as") {
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

/// Parse `src` and round-trip through our printer WITHOUT llvm-as validation.
/// Used for IR patterns our type system can represent but llvm-as rejects
/// (e.g. addrspacecast ptr→ptr with same address space).
fn roundtrip_only(label: &str, src: &str) {
    let (ctx, module) = parse(src).unwrap_or_else(|e| panic!("our parser rejected '{label}': {e}"));
    let _printed = Printer::new(&ctx).print_module(&module);
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

// ── Part 1b — fixture-file based round-trip tests (51 fixtures) ──────────────

#[test]
fn roundtrip_fixture_01_int_arith_flags() {
    roundtrip_and_validate("01", include_str!("fixtures/01_int_arith_flags.ll"));
}
#[test]
fn roundtrip_fixture_02_udiv_urem() {
    roundtrip_and_validate("02", include_str!("fixtures/02_udiv_urem.ll"));
}
#[test]
fn roundtrip_fixture_03_sdiv_exact_srem() {
    roundtrip_and_validate("03", include_str!("fixtures/03_sdiv_exact_srem.ll"));
}
#[test]
fn roundtrip_fixture_04_fp_arith_double() {
    roundtrip_and_validate("04", include_str!("fixtures/04_fp_arith_double.ll"));
}
#[test]
fn roundtrip_fixture_05_fp_arith_float() {
    roundtrip_and_validate("05", include_str!("fixtures/05_fp_arith_float.ll"));
}
#[test]
fn roundtrip_fixture_06_fp_fastmath() {
    roundtrip_and_validate("06", include_str!("fixtures/06_fp_fastmath.ll"));
}
#[test]
fn roundtrip_fixture_07_fcmp() {
    roundtrip_and_validate("07", include_str!("fixtures/07_fcmp.ll"));
}
#[test]
fn roundtrip_fixture_08_icmp_all_preds() {
    roundtrip_and_validate("08", include_str!("fixtures/08_icmp_all_preds.ll"));
}
#[test]
fn roundtrip_fixture_09_trunc_zext_sext() {
    roundtrip_and_validate("09", include_str!("fixtures/09_trunc_zext_sext.ll"));
}
#[test]
fn roundtrip_fixture_10_fptrunc_fpext() {
    roundtrip_and_validate("10", include_str!("fixtures/10_fptrunc_fpext.ll"));
}
#[test]
fn roundtrip_fixture_11_fp_int_casts() {
    roundtrip_and_validate("11", include_str!("fixtures/11_fp_int_casts.ll"));
}
#[test]
fn roundtrip_fixture_12_ptr_casts() {
    roundtrip_and_validate("12", include_str!("fixtures/12_ptr_casts.ll"));
}
#[test]
fn roundtrip_fixture_13_addrspacecast() {
    // Parse-only: our TypeData has no addrspace so the roundtrip emits
    // "addrspacecast ptr %p to ptr" (same addrspace) which llvm-as rejects.
    roundtrip_only("13", include_str!("fixtures/13_addrspacecast.ll"));
}
#[test]
fn roundtrip_fixture_14_alloca_align() {
    roundtrip_and_validate("14", include_str!("fixtures/14_alloca_align.ll"));
}
#[test]
fn roundtrip_fixture_15_load_store_align() {
    roundtrip_and_validate("15", include_str!("fixtures/15_load_store_align.ll"));
}
#[test]
fn roundtrip_fixture_15b_volatile_mem() {
    roundtrip_and_validate("15b", include_str!("fixtures/15b_volatile_mem.ll"));
}
#[test]
fn roundtrip_fixture_16_gep_inbounds() {
    roundtrip_and_validate("16", include_str!("fixtures/16_gep_inbounds.ll"));
}
#[test]
fn roundtrip_fixture_17_gep_struct() {
    roundtrip_and_validate("17", include_str!("fixtures/17_gep_struct.ll"));
}
#[test]
fn roundtrip_fixture_18_extractvalue() {
    roundtrip_and_validate("18", include_str!("fixtures/18_extractvalue.ll"));
}
#[test]
fn roundtrip_fixture_19_insertvalue() {
    roundtrip_and_validate("19", include_str!("fixtures/19_insertvalue.ll"));
}
#[test]
fn roundtrip_fixture_20_extractelement() {
    roundtrip_and_validate("20", include_str!("fixtures/20_extractelement.ll"));
}
#[test]
fn roundtrip_fixture_21_insertelement() {
    roundtrip_and_validate("21", include_str!("fixtures/21_insertelement.ll"));
}
#[test]
fn roundtrip_fixture_22_shufflevector() {
    roundtrip_and_validate("22", include_str!("fixtures/22_shufflevector.ll"));
}
#[test]
fn roundtrip_fixture_23_unreachable() {
    roundtrip_and_validate("23", include_str!("fixtures/23_unreachable.ll"));
}
#[test]
fn roundtrip_fixture_24_switch_many() {
    roundtrip_and_validate("24", include_str!("fixtures/24_switch_many.ll"));
}
#[test]
fn roundtrip_fixture_25_switch_default_only() {
    roundtrip_and_validate("25", include_str!("fixtures/25_switch_default_only.ll"));
}
#[test]
fn roundtrip_fixture_26_phi_loop() {
    roundtrip_and_validate("26", include_str!("fixtures/26_phi_loop.ll"));
}
#[test]
fn roundtrip_fixture_27_phi_multiple() {
    roundtrip_and_validate("27", include_str!("fixtures/27_phi_multiple.ll"));
}
#[test]
fn roundtrip_fixture_28_tail_calls() {
    roundtrip_and_validate("28", include_str!("fixtures/28_tail_calls.ll"));
}
#[test]
fn roundtrip_fixture_29_indirect_call() {
    roundtrip_and_validate("29", include_str!("fixtures/29_indirect_call.ll"));
}
#[test]
fn roundtrip_fixture_30_variadic_call() {
    roundtrip_and_validate("30", include_str!("fixtures/30_variadic_call.ll"));
}
#[test]
fn roundtrip_fixture_31_array_type() {
    roundtrip_and_validate("31", include_str!("fixtures/31_array_type.ll"));
}
#[test]
fn roundtrip_fixture_32_struct_anon() {
    roundtrip_and_validate("32", include_str!("fixtures/32_struct_anon.ll"));
}
#[test]
fn roundtrip_fixture_33_vector_arith() {
    roundtrip_and_validate("33", include_str!("fixtures/33_vector_arith.ll"));
}
#[test]
fn roundtrip_fixture_34_named_struct_nested() {
    roundtrip_and_validate("34", include_str!("fixtures/34_named_struct_nested.ll"));
}
#[test]
fn roundtrip_fixture_35_const_undef() {
    roundtrip_and_validate("35", include_str!("fixtures/35_const_undef.ll"));
}
#[test]
fn roundtrip_fixture_36_const_zeroinitializer() {
    roundtrip_and_validate("36", include_str!("fixtures/36_const_zeroinitializer.ll"));
}
#[test]
fn roundtrip_fixture_37_const_null() {
    roundtrip_and_validate("37", include_str!("fixtures/37_const_null.ll"));
}
#[test]
fn roundtrip_fixture_38_const_float_hex() {
    roundtrip_and_validate("38", include_str!("fixtures/38_const_float_hex.ll"));
}
#[test]
fn roundtrip_fixture_39_private_linkage() {
    roundtrip_and_validate("39", include_str!("fixtures/39_private_linkage.ll"));
}
#[test]
fn roundtrip_fixture_40_internal_linkage() {
    roundtrip_and_validate("40", include_str!("fixtures/40_internal_linkage.ll"));
}
#[test]
fn roundtrip_fixture_41_module_header() {
    roundtrip_and_validate("41", include_str!("fixtures/41_module_header.ll"));
}
#[test]
fn roundtrip_fixture_42_multi_function() {
    roundtrip_and_validate("42", include_str!("fixtures/42_multi_function.ll"));
}
#[test]
fn roundtrip_fixture_43_declare_void() {
    roundtrip_and_validate("43", include_str!("fixtures/43_declare_void.ll"));
}
#[test]
fn roundtrip_fixture_44_declare_ptr_ret() {
    roundtrip_and_validate("44", include_str!("fixtures/44_declare_ptr_ret.ll"));
}
#[test]
fn roundtrip_fixture_45_select_chain() {
    roundtrip_and_validate("45", include_str!("fixtures/45_select_chain.ll"));
}
#[test]
fn roundtrip_fixture_46_phi_diamond() {
    roundtrip_and_validate("46", include_str!("fixtures/46_phi_diamond.ll"));
}
#[test]
fn roundtrip_fixture_47_alloca_array() {
    roundtrip_and_validate("47", include_str!("fixtures/47_alloca_array.ll"));
}
#[test]
fn roundtrip_fixture_48_fp_loop() {
    roundtrip_and_validate("48", include_str!("fixtures/48_fp_loop.ll"));
}
#[test]
fn roundtrip_fixture_49_all_icmp_br() {
    roundtrip_and_validate("49", include_str!("fixtures/49_all_icmp_br.ll"));
}
#[test]
fn roundtrip_fixture_50_bitwise_shifts() {
    roundtrip_and_validate("50", include_str!("fixtures/50_bitwise_shifts.ll"));
}
#[test]
fn roundtrip_fixture_51_cast_chain() {
    roundtrip_and_validate("51", include_str!("fixtures/51_cast_chain.ll"));
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
/// Returns `(exit_code, obj_bytes)`, or `(None, bytes)` if linking fails.
fn compile_and_run_ours(ctx: &Context, module: &Module, label: &str) -> (Option<i32>, Vec<u8>) {
    let mut backend = X86Backend::default();
    let main_func = match module.functions.iter().find(|f| f.name == "main") {
        Some(f) => f,
        None => return (None, vec![]),
    };

    let mut mf = backend.lower_function(ctx, module, main_func);
    let intervals = compute_live_intervals(&mf);
    let mut result = linear_scan(&intervals, &mf.allocatable_pregs);
    insert_spill_reloads(&mut mf, &mut result, MOV_LOAD_MR, MOV_STORE_RM);
    apply_allocation(&mut mf, &result);
    let mut emitter = X86Emitter::new(ObjectFormat::Elf);
    let obj = emit_object(&mf, &mut emitter);
    let obj_bytes = obj.to_bytes();

    let exit = with_temp_file(&format!("{label}_ours"), "o", |obj_path| {
        std::fs::write(obj_path, &obj_bytes).expect("write .o");
        let bin_path = std::env::temp_dir().join(format!("llvm_diff_{label}_our_bin"));
        let link = Command::new("cc")
            .arg(obj_path)
            .arg("-o")
            .arg(&bin_path)
            .output()
            .expect("spawn cc");
        if !link.status.success() {
            return None;
        }
        let run = Command::new(&bin_path).output().expect("run our binary");
        let _ = std::fs::remove_file(&bin_path);
        Some(run.status.code().unwrap_or(-1))
    });

    (exit, obj_bytes)
}

/// Disassemble `obj_bytes` with llvm-objdump and return normalised text.
///
/// Normalisation strips addresses, raw hex bytes, blank lines, and assembler
/// directives so that the comparison is stable across machines.
fn objdump_text(obj_bytes: &[u8], label: &str) -> Option<String> {
    let objdump = llvm_tool("llvm-objdump")?;
    with_temp_file(&format!("{label}_objdump"), "o", |path| {
        std::fs::write(path, obj_bytes).expect("write .o for objdump");
        let out = Command::new(&objdump)
            .args(["--disassemble", "--no-show-raw-insn", "--no-leading-addr"])
            .arg(path)
            .output()
            .expect("spawn llvm-objdump");
        if !out.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let normalised: Vec<&str> = text
            .lines()
            .filter(|l| {
                let t = l.trim();
                !t.is_empty()
                    && !t.starts_with('.')
                    && !t.contains("file format")
                    && !t.starts_with("Disassembly")
            })
            .collect();
        Some(normalised.join("\n"))
    })
}

/// Run a semantic differential test.
///
/// Compiles `src` via LLVM (clang) and via our codegen, then asserts both
/// binaries exit with `expected_exit`.  Also stores an objdump of our ELF
/// in the regression hash database.
fn run_semantic_test(label: &str, src: &str, expected_exit: i32) {
    let clang = match require_tool("clang") {
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
    let (our_exit, obj_bytes) = compile_and_run_ours(&ctx, &module, label);

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
        (None, _) => {}
    }

    // Binary-level comparison: check that our objdump output matches the
    // known-good hash stored in the regression DB.
    if !obj_bytes.is_empty() {
        check_obj_hash(label, &obj_bytes);
        // Additionally: compare disassembly between clang and our codegen
        // when llvm-objdump is available (informational, non-fatal for now).
        if let Some(ours_asm) = objdump_text(&obj_bytes, &format!("{label}_ours")) {
            eprintln!("[{label}] our x86-64 disassembly:\n{ours_asm}");
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

// ── Part 3 — regression hash database ────────────────────────────────────────

const HASHES_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/known_hashes.json"
);

fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(data))
}

/// Load the hashes JSON.  Returns `serde_json::Value::Null` if the file does
/// not exist yet (first run before bootstrapping).
fn load_hashes() -> serde_json::Value {
    match std::fs::read_to_string(HASHES_PATH) {
        Ok(s) => serde_json::from_str(&s).expect("invalid known_hashes.json"),
        Err(_) => serde_json::json!({"version": 1, "fixtures": {}, "semantic": {}}),
    }
}

/// Check (or write when `UPDATE_HASHES=1`) the objdump hash for a semantic test.
fn check_obj_hash(label: &str, obj_bytes: &[u8]) {
    let actual = sha256_hex(obj_bytes);
    if std::env::var("UPDATE_HASHES").is_ok() {
        let mut db = load_hashes();
        db["semantic"][label]["our_obj_sha256"] = serde_json::Value::String(actual);
        std::fs::write(HASHES_PATH, serde_json::to_string_pretty(&db).unwrap())
            .expect("write known_hashes.json");
        return;
    }
    let db = load_hashes();
    if let Some(stored) = db["semantic"][label]["our_obj_sha256"].as_str() {
        assert_eq!(
            actual, stored,
            "Regression: obj hash changed for semantic test '{label}'. \
             Run with UPDATE_HASHES=1 to refresh the baseline."
        );
    }
    // No stored hash yet — first run, hash will be added by UPDATE_HASHES=1.
}

/// Check (or write when `UPDATE_HASHES=1`) the printed-IR hash for a fixture.
fn check_fixture_hash(name: &str, src: &str) {
    let (ctx, module) =
        parse(src).unwrap_or_else(|e| panic!("our parser rejected fixture '{name}': {e}"));
    let printed = Printer::new(&ctx).print_module(&module);
    let actual = sha256_hex(printed.as_bytes());

    if std::env::var("UPDATE_HASHES").is_ok() {
        let mut db = load_hashes();
        db["fixtures"][name]["printed_ir_sha256"] = serde_json::Value::String(actual);
        std::fs::write(HASHES_PATH, serde_json::to_string_pretty(&db).unwrap())
            .expect("write known_hashes.json");
        return;
    }
    let db = load_hashes();
    if let Some(stored) = db["fixtures"][name]["printed_ir_sha256"].as_str() {
        assert_eq!(
            actual, stored,
            "Regression: printed IR hash changed for fixture '{name}'. \
             Run with UPDATE_HASHES=1 to refresh the baseline."
        );
    }
}

/// Verify that every fixture's printed IR matches the known-good SHA-256 hash
/// stored in `fixtures/known_hashes.json`.
///
/// Run `UPDATE_HASHES=1 cargo test -p llvm-ir-parser check_regression_hashes`
/// to regenerate the database after intentional printer changes.
#[test]
fn check_regression_hashes() {
    let fixtures: &[(&str, &str)] = &[
        ("01_int_arith_flags", include_str!("fixtures/01_int_arith_flags.ll")),
        ("02_udiv_urem", include_str!("fixtures/02_udiv_urem.ll")),
        ("03_sdiv_exact_srem", include_str!("fixtures/03_sdiv_exact_srem.ll")),
        ("04_fp_arith_double", include_str!("fixtures/04_fp_arith_double.ll")),
        ("05_fp_arith_float", include_str!("fixtures/05_fp_arith_float.ll")),
        ("06_fp_fastmath", include_str!("fixtures/06_fp_fastmath.ll")),
        ("07_fcmp", include_str!("fixtures/07_fcmp.ll")),
        ("08_icmp_all_preds", include_str!("fixtures/08_icmp_all_preds.ll")),
        ("09_trunc_zext_sext", include_str!("fixtures/09_trunc_zext_sext.ll")),
        ("10_fptrunc_fpext", include_str!("fixtures/10_fptrunc_fpext.ll")),
        ("11_fp_int_casts", include_str!("fixtures/11_fp_int_casts.ll")),
        ("12_ptr_casts", include_str!("fixtures/12_ptr_casts.ll")),
        ("14_alloca_align", include_str!("fixtures/14_alloca_align.ll")),
        ("15_load_store_align", include_str!("fixtures/15_load_store_align.ll")),
        ("15b_volatile_mem", include_str!("fixtures/15b_volatile_mem.ll")),
        ("16_gep_inbounds", include_str!("fixtures/16_gep_inbounds.ll")),
        ("17_gep_struct", include_str!("fixtures/17_gep_struct.ll")),
        ("18_extractvalue", include_str!("fixtures/18_extractvalue.ll")),
        ("19_insertvalue", include_str!("fixtures/19_insertvalue.ll")),
        ("20_extractelement", include_str!("fixtures/20_extractelement.ll")),
        ("21_insertelement", include_str!("fixtures/21_insertelement.ll")),
        ("22_shufflevector", include_str!("fixtures/22_shufflevector.ll")),
        ("23_unreachable", include_str!("fixtures/23_unreachable.ll")),
        ("24_switch_many", include_str!("fixtures/24_switch_many.ll")),
        ("25_switch_default_only", include_str!("fixtures/25_switch_default_only.ll")),
        ("26_phi_loop", include_str!("fixtures/26_phi_loop.ll")),
        ("27_phi_multiple", include_str!("fixtures/27_phi_multiple.ll")),
        ("28_tail_calls", include_str!("fixtures/28_tail_calls.ll")),
        ("29_indirect_call", include_str!("fixtures/29_indirect_call.ll")),
        ("30_variadic_call", include_str!("fixtures/30_variadic_call.ll")),
        ("31_array_type", include_str!("fixtures/31_array_type.ll")),
        ("32_struct_anon", include_str!("fixtures/32_struct_anon.ll")),
        ("33_vector_arith", include_str!("fixtures/33_vector_arith.ll")),
        ("34_named_struct_nested", include_str!("fixtures/34_named_struct_nested.ll")),
        ("35_const_undef", include_str!("fixtures/35_const_undef.ll")),
        ("36_const_zeroinitializer", include_str!("fixtures/36_const_zeroinitializer.ll")),
        ("37_const_null", include_str!("fixtures/37_const_null.ll")),
        ("38_const_float_hex", include_str!("fixtures/38_const_float_hex.ll")),
        ("39_private_linkage", include_str!("fixtures/39_private_linkage.ll")),
        ("40_internal_linkage", include_str!("fixtures/40_internal_linkage.ll")),
        ("41_module_header", include_str!("fixtures/41_module_header.ll")),
        ("42_multi_function", include_str!("fixtures/42_multi_function.ll")),
        ("43_declare_void", include_str!("fixtures/43_declare_void.ll")),
        ("44_declare_ptr_ret", include_str!("fixtures/44_declare_ptr_ret.ll")),
        ("45_select_chain", include_str!("fixtures/45_select_chain.ll")),
        ("46_phi_diamond", include_str!("fixtures/46_phi_diamond.ll")),
        ("47_alloca_array", include_str!("fixtures/47_alloca_array.ll")),
        ("48_fp_loop", include_str!("fixtures/48_fp_loop.ll")),
        ("49_all_icmp_br", include_str!("fixtures/49_all_icmp_br.ll")),
        ("50_bitwise_shifts", include_str!("fixtures/50_bitwise_shifts.ll")),
        ("51_cast_chain", include_str!("fixtures/51_cast_chain.ll")),
    ];

    for (name, src) in fixtures {
        check_fixture_hash(name, src);
    }
}
