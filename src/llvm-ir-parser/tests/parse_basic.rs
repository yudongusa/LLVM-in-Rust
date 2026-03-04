//! Integration tests: parse representative `.ll` snippets and assert structure.

use llvm_ir_parser::parser::parse;

/// Verify that a minimal function with only `ret void` parses correctly.
#[test]
fn parse_minimal_function() {
    let src = r#"
define void @noop() {
entry:
  ret void
}
"#;
    let (_ctx, module) = parse(src).expect("parse failed");
    assert_eq!(module.functions.len(), 1);
    let f = &module.functions[0];
    assert_eq!(f.name, "noop");
    assert!(!f.is_declaration);
    assert_eq!(f.blocks.len(), 1);
    assert_eq!(f.blocks[0].name, "entry");
    assert!(f.blocks[0].is_complete());
}

/// Parse a simple arithmetic function.
#[test]
fn parse_arithmetic() {
    let src = r#"
define i32 @mul(i32 %a, i32 %b) {
entry:
  %r = mul i32 %a, %b
  ret i32 %r
}
"#;
    let (_ctx, module) = parse(src).expect("parse failed");
    let f = &module.functions[0];
    assert_eq!(f.name, "mul");
    assert_eq!(f.args.len(), 2);
    assert_eq!(f.args[0].name, "a");
    assert_eq!(f.args[1].name, "b");
    let bb = &f.blocks[0];
    // body has 1 instruction (mul), terminator is ret
    assert_eq!(bb.body.len(), 1);
    assert!(bb.terminator.is_some());
}

/// Parse a function declaration (no body).
#[test]
fn parse_declaration_variadic() {
    let src = "declare i32 @printf(ptr, ...)";
    let (_ctx, module) = parse(src).expect("parse failed");
    assert_eq!(module.functions.len(), 1);
    let f = &module.functions[0];
    assert!(f.is_declaration);
    assert_eq!(f.name, "printf");
}

/// Parse module metadata.
#[test]
fn parse_module_metadata() {
    let src = r#"
source_filename = "hello.c"
target triple = "aarch64-apple-darwin"
target datalayout = "e-m:o-i64:64-i128:128-n32:64-S128"
"#;
    let (_ctx, module) = parse(src).expect("parse failed");
    assert_eq!(module.source_filename.as_deref(), Some("hello.c"));
    assert_eq!(
        module.target_triple.as_deref(),
        Some("aarch64-apple-darwin")
    );
    assert!(module.data_layout.is_some());
}

/// Parse a global variable.
#[test]
fn parse_global_variable() {
    let src = "@count = global i32 0";
    let (_ctx, module) = parse(src).expect("parse failed");
    assert_eq!(module.globals.len(), 1);
    assert_eq!(module.globals[0].name, "count");
    assert!(!module.globals[0].is_constant);
}

/// Parse a private constant global.
#[test]
fn parse_constant_global() {
    let src = "@msg = private constant i8 65";
    let (_ctx, module) = parse(src).expect("parse failed");
    let gv = &module.globals[0];
    assert_eq!(gv.name, "msg");
    assert!(gv.is_constant);
}

/// Parse alloca / load / store sequence.
#[test]
fn parse_alloca_load_store() {
    let src = r#"
define void @f(i32 %v) {
entry:
  %slot = alloca i32
  store i32 %v, ptr %slot
  %loaded = load i32, ptr %slot
  ret void
}
"#;
    let (_ctx, module) = parse(src).expect("parse failed");
    let f = &module.functions[0];
    assert_eq!(f.blocks[0].body.len(), 3); // alloca, store, load
}

/// Parse icmp + conditional branch across two blocks.
#[test]
fn parse_icmp_cond_br() {
    let src = r#"
define i32 @abs(i32 %n) {
entry:
  %cmp = icmp sge i32 %n, 0
  br i1 %cmp, label %pos, label %neg
pos:
  ret i32 %n
neg:
  %r = sub i32 0, %n
  ret i32 %r
}
"#;
    let (_ctx, module) = parse(src).expect("parse failed");
    let f = &module.functions[0];
    assert_eq!(f.blocks.len(), 3);
    assert_eq!(f.blocks[0].name, "entry");
    // The pos and neg blocks may be in any order due to forward-ref allocation.
    let names: Vec<&str> = f.blocks.iter().map(|b| b.name.as_str()).collect();
    assert!(names.contains(&"pos"));
    assert!(names.contains(&"neg"));
}

/// Parse a phi node.
#[test]
fn parse_phi() {
    let src = r#"
define i32 @phi_test(i1 %cond) {
entry:
  br i1 %cond, label %a, label %b
a:
  br label %merge
b:
  br label %merge
merge:
  %v = phi i32 [ 1, %a ], [ 2, %b ]
  ret i32 %v
}
"#;
    let (_ctx, module) = parse(src).expect("parse failed");
    let f = &module.functions[0];
    // Should have entry, a, b, merge
    assert!(f.blocks.len() >= 4);
}

/// Parse a named struct type.
#[test]
fn parse_named_struct() {
    let src = r#"
%Point = type { i32, i32 }
"#;
    let (_ctx, module) = parse(src).expect("parse failed");
    assert_eq!(module.named_types.len(), 1);
    assert_eq!(module.named_types[0].0, "Point");
}
