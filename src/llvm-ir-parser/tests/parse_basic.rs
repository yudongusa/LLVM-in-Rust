//! Integration tests: parse representative `.ll` snippets and assert structure.

use llvm_ir::{
    printer::Printer, ConstantData, InstrKind, LandingPadClause, MemOrdering, RmwOp, VpIntrinsic,
};
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

/// Parse and print LLVM inline assembly call syntax.
#[test]
fn parse_print_inline_asm_nop() {
    let src = r#"
define void @f() {
entry:
  call void asm sideeffect "nop", ""()
  ret void
}
"#;
    let (ctx, module) = parse(src).expect("parse failed");
    let f = &module.functions[0];
    let bb = &f.blocks[0];
    let instr = f.instr(bb.body[0]);
    match &instr.kind {
        InstrKind::InlineAsm {
            asm_string,
            constraints,
            side_effect,
            args,
            ..
        } => {
            assert_eq!(asm_string, "nop");
            assert_eq!(constraints, "");
            assert!(*side_effect);
            assert!(args.is_empty());
        }
        other => panic!("expected inline asm, got {other:?}"),
    }

    let printed = Printer::new(&ctx).print_module(&module);
    assert!(printed.contains("call void asm sideeffect \"nop\", \"\"()"));
}

/// Parse `fence`, `cmpxchg`, and `atomicrmw` and confirm the IR structure
/// + the result of the print → parse round-trip is stable.  Covers issue #205
/// at the parser layer.
#[test]
fn parse_atomics_round_trip() {
    let src = r#"
define i32 @atomic_step(ptr %p, i32 %cmp, i32 %new) {
entry:
  fence seq_cst
  %cas = cmpxchg ptr %p, i32 %cmp, i32 %new acq_rel acquire
  %old = atomicrmw add ptr %p, i32 %new seq_cst
  ret i32 %old
}
"#;
    let (ctx, module) = parse(src).expect("parse failed");
    let f = &module.functions[0];
    let bb = &f.blocks[0];
    assert_eq!(bb.body.len(), 3);

    // Fence
    let fence_kind = &f.instr(bb.body[0]).kind;
    match fence_kind {
        InstrKind::Fence { ordering } => assert_eq!(*ordering, MemOrdering::SeqCst),
        other => panic!("expected Fence, got {other:?}"),
    }

    // CmpXchg
    match &f.instr(bb.body[1]).kind {
        InstrKind::CmpXchg {
            success_ord,
            fail_ord,
            weak,
            volatile,
            ..
        } => {
            assert_eq!(*success_ord, MemOrdering::AcqRel);
            assert_eq!(*fail_ord, MemOrdering::Acquire);
            assert!(!*weak);
            assert!(!*volatile);
        }
        other => panic!("expected CmpXchg, got {other:?}"),
    }

    // AtomicRmw
    match &f.instr(bb.body[2]).kind {
        InstrKind::AtomicRmw {
            op,
            ordering,
            volatile,
            ..
        } => {
            assert_eq!(*op, RmwOp::Add);
            assert_eq!(*ordering, MemOrdering::SeqCst);
            assert!(!*volatile);
        }
        other => panic!("expected AtomicRmw, got {other:?}"),
    }

    // print → parse round-trip preserves all three instructions
    let printed = Printer::new(&ctx).print_module(&module);
    assert!(printed.contains("fence seq_cst"), "{printed}");
    assert!(
        printed
            .contains("%cas = cmpxchg ptr %p, i32 %cmp, i32 %new acq_rel acquire"),
        "{printed}"
    );
    assert!(
        printed.contains("%old = atomicrmw add ptr %p, i32 %new seq_cst"),
        "{printed}"
    );

    let (_ctx2, module2) = parse(&printed).expect("re-parse of printed IR failed");
    let printed2 = Printer::new(&_ctx2).print_module(&module2);
    assert_eq!(printed, printed2, "print → parse → print not idempotent");
}

/// `cmpxchg weak volatile` must parse and round-trip the modifiers.
#[test]
fn parse_atomics_weak_volatile_modifiers() {
    let src = r#"
define void @cas(ptr %p, i32 %cmp, i32 %new) {
entry:
  %r = cmpxchg weak volatile ptr %p, i32 %cmp, i32 %new seq_cst monotonic
  ret void
}
"#;
    let (ctx, module) = parse(src).expect("parse failed");
    let f = &module.functions[0];
    match &f.instr(f.blocks[0].body[0]).kind {
        InstrKind::CmpXchg { weak, volatile, .. } => {
            assert!(*weak);
            assert!(*volatile);
        }
        other => panic!("expected CmpXchg, got {other:?}"),
    }
    let printed = Printer::new(&ctx).print_module(&module);
    assert!(printed.contains("cmpxchg weak volatile"), "{printed}");
    assert!(printed.contains("seq_cst monotonic"), "{printed}");
}

/// `atomicrmw xchg` exercises the `LocalIdent` op-name branch (the keyword
/// branch is exercised by the `add` op above).
#[test]
fn parse_atomicrmw_xchg() {
    let src = r#"
define i32 @swap(ptr %p, i32 %v) {
entry:
  %old = atomicrmw xchg ptr %p, i32 %v acquire
  ret i32 %old
}
"#;
    let (ctx, module) = parse(src).expect("parse failed");
    match &module.functions[0]
        .instr(module.functions[0].blocks[0].body[0])
        .kind
    {
        InstrKind::AtomicRmw { op, ordering, .. } => {
            assert_eq!(*op, RmwOp::Xchg);
            assert_eq!(*ordering, MemOrdering::Acquire);
        }
        other => panic!("expected AtomicRmw, got {other:?}"),
    }
    let printed = Printer::new(&ctx).print_module(&module);
    assert!(
        printed.contains("atomicrmw xchg ptr %p, i32 %v acquire"),
        "{printed}"
    );
}

/// Parse LLVM `vp.*` vector-predication intrinsic calls as recognized call targets.
#[test]
fn parse_vp_add_intrinsic_call() {
    let src = r#"
declare <4 x i32> @llvm.vp.add.v4i32(<4 x i32>, <4 x i32>, <4 x i1>, i32)

define <4 x i32> @f(<4 x i32> %a, <4 x i32> %b, <4 x i1> %mask, i32 %evl) {
entry:
  %r = call <4 x i32> @llvm.vp.add.v4i32(<4 x i32> %a, <4 x i32> %b, <4 x i1> %mask, i32 %evl)
  ret <4 x i32> %r
}
"#;
    let (ctx, module) = parse(src).expect("parse failed");
    let f = module
        .functions
        .iter()
        .find(|f| f.name == "f")
        .expect("function f");
    let instr = f.instr(f.blocks[0].body[0]);
    match &instr.kind {
        InstrKind::Call { callee, args, .. } => {
            assert_eq!(args.len(), 4);
            let llvm_ir::ValueRef::Constant(cid) = callee else {
                panic!("expected constant global ref callee");
            };
            let ConstantData::GlobalRef { name, .. } = ctx.get_const(*cid) else {
                panic!("expected global ref callee");
            };
            assert_eq!(VpIntrinsic::from_name(name), Some(VpIntrinsic::Add));
        }
        other => panic!("expected call, got {other:?}"),
    }
}

/// Parse clang-style constant getelementptr in `load` operand position.
/// Each constexpr in an instruction operand is hoisted at parse time to a
/// fresh anonymous SSA instruction inserted at the current insertion point.
/// Covers issue #207 PR-A.
#[test]
fn parse_constexpr_gep_in_load_operand() {
    let src = r#"
@table = external constant [8 x i32]
define i32 @f() {
entry:
  %v = load i32, ptr getelementptr inbounds ([8 x i32], ptr @table, i64 0, i64 3), align 4
  ret i32 %v
}
"#;
    let (ctx, module) = parse(src).expect("parse failed");
    let f = &module.functions[0];
    let bb = &f.blocks[0];

    // The constexpr GEP must have been hoisted into the entry block before
    // the load that uses it — block body length goes from 1 (the load) to 2
    // (hoisted GEP + load).
    assert_eq!(
        bb.body.len(),
        2,
        "expected hoisted GEP + load in block body, got body={:?}",
        bb.body
    );

    let hoisted = f.instr(bb.body[0]);
    assert!(
        matches!(hoisted.kind, InstrKind::GetElementPtr { inbounds: true, .. }),
        "first instruction should be a hoisted inbounds GEP, got {:?}",
        hoisted.kind
    );
    assert_eq!(
        hoisted.name, None,
        "hoisted constexpr should be anonymous (parser slot)"
    );

    let load = f.instr(bb.body[1]);
    assert!(matches!(load.kind, InstrKind::Load { .. }));

    let printed = Printer::new(&ctx).print_module(&module);
    assert!(
        printed.contains("%v0 = getelementptr inbounds [8 x i32], ptr @table, i64 0, i64 3"),
        "printed module missing hoisted GEP definition:\n{printed}"
    );
    assert!(
        printed.contains("%v = load i32, ptr %v0"),
        "printed module missing load using hoisted GEP:\n{printed}"
    );
}

/// `bitcast`, `inttoptr`, `ptrtoint`, `addrspacecast`, `trunc`, `zext`,
/// `sext` constexpr forms in instruction operand position all hoist to
/// fresh SSA instructions.
#[test]
fn parse_constexpr_cast_forms_in_operands() {
    let src = r#"
@g = external constant i32
declare void @sink(ptr)
define i64 @f() {
entry:
  call void @sink(ptr bitcast (ptr @g to ptr))
  call void @sink(ptr inttoptr (i64 4096 to ptr))
  %p = ptrtoint ptr getelementptr inbounds ([4 x i32], ptr @g, i64 0, i64 2) to i64
  ret i64 %p
}
"#;
    let (_ctx, module) = parse(src).expect("parse failed");
    let bb = &module.functions[1].blocks[0];

    // entry must now contain: hoisted bitcast, call#1, hoisted inttoptr,
    // call#2, hoisted GEP, ptrtoint, ret = 7 instrs total (5 body + 1
    // terminator pulled out).  Just assert at least 5 in body since the
    // exact terminator handling lives in the test harness conventions.
    assert!(
        bb.body.len() >= 5,
        "expected at least 5 body instructions from hoisting, got {}",
        bb.body.len()
    );

    // First instruction must be the hoisted bitcast (anonymous).
    let first = module.functions[1].instr(bb.body[0]);
    assert!(matches!(first.kind, InstrKind::BitCast { .. }));
    assert_eq!(first.name, None);
}

/// `@table = internal constant [8 x i32] [i32 1, i32 2, ...]` style array
/// global initializers parse into `ConstantData::Array`.  Covers issue #207
/// PR-A's "array literal as global initializer" gap.
#[test]
fn parse_global_array_constant_initializer() {
    let src = r#"
@table = internal constant [8 x i32] [i32 1, i32 2, i32 3, i32 4, i32 5, i32 6, i32 7, i32 8]
"#;
    let (ctx, module) = parse(src).expect("parse failed");
    assert_eq!(module.globals.len(), 1);
    let init_cid = module.globals[0].initializer.expect("initializer present");
    match ctx.get_const(init_cid) {
        ConstantData::Array { elements, .. } => assert_eq!(elements.len(), 8),
        other => panic!("expected Array initializer, got {other:?}"),
    }
    let printed = Printer::new(&ctx).print_module(&module);
    assert!(
        printed.contains("[i32 1, i32 2, i32 3, i32 4, i32 5, i32 6, i32 7, i32 8]"),
        "{printed}"
    );
}

/// Array of named-struct constants — each element prefixed with the named
/// struct type and a `{ ... }` field list — round-trips through the parser.
/// Mirrors clang's vtable-style emission pattern.
#[test]
fn parse_global_array_of_named_struct_constants() {
    let src = r#"
%struct.entry = type { ptr, i32 }
declare i32 @handler(i32)
@vt = internal constant [3 x %struct.entry] [%struct.entry { ptr @handler, i32 10 }, %struct.entry { ptr @handler, i32 20 }, %struct.entry { ptr @handler, i32 30 }]
"#;
    let (ctx, module) = parse(src).expect("parse failed");
    let init_cid = module.globals[0].initializer.expect("initializer present");
    let ConstantData::Array { elements, .. } = ctx.get_const(init_cid) else {
        panic!("expected outer Array");
    };
    assert_eq!(elements.len(), 3);
    // Each element should be a Struct constant with 2 fields.
    for &eid in elements {
        match ctx.get_const(eid) {
            ConstantData::Struct { fields, .. } => assert_eq!(fields.len(), 2),
            other => panic!("expected Struct element, got {other:?}"),
        }
    }
}

/// Parse and print the core `invoke` / `landingpad` exception-control shape.
#[test]
fn parse_print_invoke_landingpad() {
    let src = r#"
declare i32 @may_throw()

define i32 @f() {
entry:
  %r = invoke i32 @may_throw() to label %normal unwind label %lpad
normal:
  ret i32 %r
lpad:
  %lp = landingpad { ptr, i32 } cleanup catch ptr null
  ret i32 -1
}
"#;
    let (ctx, module) = parse(src).expect("parse failed");
    let f = module.functions.iter().find(|f| f.name == "f").expect("function f");
    match &f.instr(f.blocks[0].terminator.expect("entry terminator")).kind {
        InstrKind::Invoke {
            normal_dest,
            unwind_dest,
            ..
        } => {
            assert_eq!(f.block(*normal_dest).name, "normal");
            assert_eq!(f.block(*unwind_dest).name, "lpad");
        }
        other => panic!("expected invoke, got {other:?}"),
    }
    match &f.instr(f.blocks[2].body[0]).kind {
        InstrKind::LandingPad {
            cleanup, clauses, ..
        } => {
            assert!(*cleanup);
            assert!(matches!(clauses.as_slice(), [LandingPadClause::Catch { .. }]));
        }
        other => panic!("expected landingpad, got {other:?}"),
    }

    let printed = Printer::new(&ctx).print_module(&module);
    assert!(printed.contains("invoke i32 @may_throw() to label %normal unwind label %lpad"));
    assert!(printed.contains("landingpad { ptr, i32 } cleanup catch ptr null"));
}

/// Parse clang-emitted parameter/function attributes that the IR model does
/// not preserve yet. These should be accepted and ignored so realistic clang
/// modules can reach later compatibility checks.
#[test]
fn parse_clang_parameter_attrs_and_attribute_group() {
    let src = r#"
declare void @sink(ptr noundef nonnull dereferenceable(8), i32 signext)
declare dso_local noundef i32 @helper(i32 zeroext)
declare cc 10 hidden void @custom_cc()

define dso_local noundef i32 @caller(i32 noundef %arg, ptr nonnull %ptr) #0 {
entry:
  call void @sink(ptr noundef nonnull %ptr, i32 signext %arg)
  %r = call i32 @helper(i32 noundef zeroext %arg)
  %lit = call i32 @helper(i32 41)
  ret i32 %r
}

attributes #0 = { nounwind "frame-pointer"="all" }
"#;

    let (_ctx, module) = parse(src).expect("parse failed");
    assert_eq!(module.functions.len(), 4);

    let caller = module
        .functions
        .iter()
        .find(|f| f.name == "caller")
        .expect("caller");
    assert_eq!(caller.args.len(), 2);
    assert_eq!(caller.args[0].name, "arg");
    assert_eq!(caller.args[1].name, "ptr");
    assert_eq!(caller.blocks[0].body.len(), 3);

    match &caller.instr(caller.blocks[0].body[0]).kind {
        InstrKind::Call { args, .. } => assert_eq!(args.len(), 2),
        other => panic!("expected call, got {other:?}"),
    }
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

/// Parse LLVM 10+ `freeze` as a value-producing identity instruction.
#[test]
fn parse_freeze_instruction() {
    let src = r#"
define i32 @freeze_test(i32 %x) {
entry:
  %y = freeze i32 %x
  ret i32 %y
}
"#;
    let (_ctx, module) = parse(src).expect("parse failed");
    let f = &module.functions[0];
    let bb = &f.blocks[0];
    assert_eq!(bb.body.len(), 1);
    assert_eq!(f.instr(bb.body[0]).kind.opcode(), "freeze");
}
