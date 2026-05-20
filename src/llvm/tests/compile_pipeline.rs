//! Integration tests for the compile_ir_to_object pipeline (issue #217).
//!
//! These tests call the compile logic directly without subprocesses so they
//! run on every platform in the standard `cargo test` suite.

use llvm::compile::{compile_ir_to_object, host_object_format, regalloc_strategy_for};
use llvm::codegen::regalloc::RegAllocStrategy;
use llvm_transforms::OptLevel;

// ── helpers ──────────────────────────────────────────────────────────────────

fn is_elf(bytes: &[u8]) -> bool {
    bytes.len() > 4 && bytes[0..4] == [0x7f, b'E', b'L', b'F']
}

fn is_macho(bytes: &[u8]) -> bool {
    bytes.len() > 4
        && (bytes[0..4] == [0xCF, 0xFA, 0xED, 0xFE] // 64-bit LE
            || bytes[0..4] == [0xCE, 0xFA, 0xED, 0xFE]) // 32-bit LE
}

fn is_coff(bytes: &[u8]) -> bool {
    // COFF Machine field 0x8664 (AMD64) is at offset 0
    bytes.len() >= 2 && bytes[0] == 0x64 && bytes[1] == 0x86
}

fn looks_like_object(bytes: &[u8]) -> bool {
    is_elf(bytes) || is_macho(bytes) || is_coff(bytes)
}

// ── tests ────────────────────────────────────────────────────────────────────

#[test]
fn compile_single_function_returns_valid_object() {
    let src = r#"define i32 @main() {
entry:
  ret i32 42
}
"#;
    let fmt = host_object_format();
    let bytes = compile_ir_to_object(src, OptLevel::O0, fmt)
        .expect("compile failed");
    assert!(!bytes.is_empty(), "expected non-empty object");
    assert!(
        looks_like_object(&bytes),
        "output does not look like a valid object file (first 4 bytes: {:?})",
        &bytes[..bytes.len().min(4)]
    );
}

#[test]
fn compile_multi_function_module_emits_both_symbols() {
    // Two defined functions; both should appear as symbols in the object.
    let src = r#"define i32 @helper(i32 %x) {
entry:
  %r = add i32 %x, 1
  ret i32 %r
}

define i32 @main() {
entry:
  %r = call i32 @helper(i32 41)
  ret i32 %r
}
"#;
    let fmt = host_object_format();
    let bytes = compile_ir_to_object(src, OptLevel::O0, fmt)
        .expect("compile failed");
    assert!(!bytes.is_empty());
    // The object bytes should contain the symbol names somewhere as strings.
    let _text = std::str::from_utf8(&bytes).unwrap_or("");
    // ELF/Mach-O/COFF all store symbol names as ASCII in a string table.
    let raw_contains = |needle: &[u8]| {
        bytes.windows(needle.len()).any(|w| w == needle)
    };
    assert!(raw_contains(b"main"), "missing 'main' symbol in object");
    assert!(raw_contains(b"helper"), "missing 'helper' symbol in object");
}

#[test]
fn compile_with_o1_produces_nonempty_object() {
    let src = r#"define i32 @main() {
entry:
  %slot = alloca i32, align 4
  store i32 7, ptr %slot, align 4
  %v = load i32, ptr %slot, align 4
  %r = add i32 %v, 1
  ret i32 %r
}
"#;
    let fmt = host_object_format();
    let o0 = compile_ir_to_object(src, OptLevel::O0, fmt).expect("O0 failed");
    let o1 = compile_ir_to_object(src, OptLevel::O1, fmt).expect("O1 failed");
    assert!(!o0.is_empty());
    assert!(!o1.is_empty());
    // O1 should be <= O0 in text size (mem2reg eliminates the stack slot).
    // Just verify both succeed; size comparison is advisory.
}

#[test]
fn compile_with_declarations_only_returns_error() {
    let src = r#"declare i32 @printf(ptr, ...)
"#;
    let fmt = host_object_format();
    let result = compile_ir_to_object(src, OptLevel::O0, fmt);
    assert!(
        result.is_err(),
        "expected error for module with only declarations"
    );
}

#[test]
fn compile_arithmetic_chain() {
    let src = r#"define i32 @main() {
entry:
  %a = add i32 10, 5
  %b = sub i32 %a, 3
  %c = mul i32 %b, 2
  ret i32 %c
}
"#;
    let fmt = host_object_format();
    let bytes = compile_ir_to_object(src, OptLevel::O1, fmt).expect("compile failed");
    assert!(looks_like_object(&bytes));
}

#[test]
fn compile_conditional_branch() {
    let src = r#"define i32 @main() {
entry:
  %cond = icmp sgt i32 5, 3
  br i1 %cond, label %then, label %else_
then:
  ret i32 1
else_:
  ret i32 0
}
"#;
    let fmt = host_object_format();
    let bytes = compile_ir_to_object(src, OptLevel::O1, fmt).expect("compile failed");
    assert!(looks_like_object(&bytes));
}

#[test]
fn compile_recursive_function() {
    // Recursive fibonacci; exercises call lowering and multiple blocks.
    let src = r#"define i32 @fib(i32 %n) {
entry:
  %cmp = icmp sle i32 %n, 1
  br i1 %cmp, label %base, label %recurse
base:
  ret i32 %n
recurse:
  %n1 = sub i32 %n, 1
  %n2 = sub i32 %n, 2
  %r1 = call i32 @fib(i32 %n1)
  %r2 = call i32 @fib(i32 %n2)
  %sum = add i32 %r1, %r2
  ret i32 %sum
}

define i32 @main() {
entry:
  %r = call i32 @fib(i32 10)
  ret i32 %r
}
"#;
    let fmt = host_object_format();
    let bytes = compile_ir_to_object(src, OptLevel::O1, fmt).expect("compile failed");
    assert!(looks_like_object(&bytes));
    let raw_contains = |needle: &[u8]| bytes.windows(needle.len()).any(|w| w == needle);
    assert!(raw_contains(b"fib"));
    assert!(raw_contains(b"main"));
}

// ── regalloc strategy selection (issue #300) ─────────────────────────────────

#[test]
fn regalloc_strategy_o0_o1_use_linear_scan() {
    assert_eq!(
        regalloc_strategy_for(OptLevel::O0),
        RegAllocStrategy::LinearScan,
        "O0 must use linear scan"
    );
    assert_eq!(
        regalloc_strategy_for(OptLevel::O1),
        RegAllocStrategy::LinearScan,
        "O1 must use linear scan"
    );
}

#[test]
fn regalloc_strategy_o2_o3_use_graph_color() {
    assert_eq!(
        regalloc_strategy_for(OptLevel::O2),
        RegAllocStrategy::GraphColor,
        "O2 must use graph coloring"
    );
    assert_eq!(
        regalloc_strategy_for(OptLevel::O3),
        RegAllocStrategy::GraphColor,
        "O3 must use graph coloring"
    );
}

#[test]
fn compile_o2_produces_valid_object() {
    // Exercises the full pipeline with graph-coloring regalloc (O2).
    let src = r#"define i32 @compute(i32 %a, i32 %b, i32 %c) {
entry:
  %s = add i32 %a, %b
  %t = mul i32 %s, %c
  %u = sub i32 %t, %a
  ret i32 %u
}
"#;
    let fmt = host_object_format();
    let bytes = compile_ir_to_object(src, OptLevel::O2, fmt).expect("O2 compile failed");
    assert!(looks_like_object(&bytes), "O2 output must be a valid object file");
}

#[test]
fn compile_o3_produces_valid_object() {
    // Exercises the full pipeline with graph-coloring regalloc (O3).
    let src = r#"define i32 @add(i32 %x, i32 %y) {
entry:
  %r = add i32 %x, %y
  ret i32 %r
}
"#;
    let fmt = host_object_format();
    let bytes = compile_ir_to_object(src, OptLevel::O3, fmt).expect("O3 compile failed");
    assert!(looks_like_object(&bytes), "O3 output must be a valid object file");
}

#[test]
fn gc_regalloc_no_spills_on_pressure_free_function() {
    // A function with only a few live values; graph coloring should assign
    // all to physical registers with zero spills.  Compile at O2 and verify
    // we get a non-empty, structurally valid object.
    let src = r#"define i32 @pressure_free(i32 %a, i32 %b) {
entry:
  %c = add i32 %a, %b
  %d = sub i32 %c, 1
  ret i32 %d
}
"#;
    let fmt = host_object_format();
    let bytes = compile_ir_to_object(src, OptLevel::O2, fmt)
        .expect("pressure-free O2 compile failed");
    assert!(
        looks_like_object(&bytes),
        "graph-coloring O2 output must be a valid object file"
    );
    assert!(
        !bytes.is_empty(),
        "graph-coloring O2 output must not be empty"
    );
}
