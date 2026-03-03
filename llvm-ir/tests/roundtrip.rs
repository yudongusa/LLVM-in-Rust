//! Round-trip test: build IR → print → parse → print → assert text equality.

use llvm_ir::{
    Context, Module, Function, BasicBlock, Instruction, InstrKind,
    Builder, Printer, Linkage, IntArithFlags, IntPredicate,
    ValueRef,
};

/// Build a simple add function and check that the printer emits expected text.
#[test]
fn roundtrip_add() {
    let mut ctx = Context::new();
    let mut module = Module::new("roundtrip");
    let mut b = Builder::new(&mut ctx, &mut module);

    let _fid = b.add_function(
        "add",
        b.ctx.i32_ty,
        vec![b.ctx.i32_ty, b.ctx.i32_ty],
        vec!["a".to_string(), "b".to_string()],
        false,
        Linkage::External,
    );
    let entry = b.add_block("entry");
    b.position_at_end(entry);

    let a = b.get_arg(0);
    let bv = b.get_arg(1);
    let sum = b.build_add("sum", a, bv);
    b.build_ret(sum);

    let p = Printer::new(b.ctx);
    let ir = p.print_module(b.module);

    assert!(ir.contains("define i32 @add("), "missing function header in:\n{}", ir);
    assert!(ir.contains("%sum = add i32 %a, %b"), "missing add in:\n{}", ir);
    assert!(ir.contains("ret i32 %sum"), "missing ret in:\n{}", ir);
}

/// Build a conditional branch function and verify structure.
#[test]
fn roundtrip_cond_br() {
    let mut ctx = Context::new();
    let mut module = Module::new("cond");
    let mut b = Builder::new(&mut ctx, &mut module);

    let _fid = b.add_function(
        "max",
        b.ctx.i32_ty,
        vec![b.ctx.i32_ty, b.ctx.i32_ty],
        vec!["x".to_string(), "y".to_string()],
        false,
        Linkage::External,
    );
    let entry   = b.add_block("entry");
    let then_bb = b.add_block("ret_x");
    let else_bb = b.add_block("ret_y");

    b.position_at_end(entry);
    let x = b.get_arg(0);
    let y = b.get_arg(1);
    let cond = b.build_icmp("cond", IntPredicate::Sgt, x, y);
    b.build_cond_br(cond, then_bb, else_bb);

    b.position_at_end(then_bb);
    b.build_ret(x);

    b.position_at_end(else_bb);
    b.build_ret(y);

    let p = Printer::new(b.ctx);
    let ir = p.print_module(b.module);

    assert!(ir.contains("define i32 @max("), "missing header:\n{}", ir);
    assert!(ir.contains("icmp sgt"), "missing icmp:\n{}", ir);
    assert!(ir.contains("br i1"), "missing br:\n{}", ir);
    assert!(ir.contains("label %ret_x"), "missing then label:\n{}", ir);
    assert!(ir.contains("label %ret_y"), "missing else label:\n{}", ir);
}

/// Build a function with alloca/load/store and verify output.
#[test]
fn roundtrip_memory() {
    let mut ctx = Context::new();
    let mut module = Module::new("mem");
    let mut b = Builder::new(&mut ctx, &mut module);

    let _fid = b.add_function(
        "swap",
        b.ctx.void_ty,
        vec![b.ctx.ptr_ty, b.ctx.ptr_ty],
        vec!["p".to_string(), "q".to_string()],
        false,
        Linkage::External,
    );
    let entry = b.add_block("entry");
    b.position_at_end(entry);

    let p_ref = b.get_arg(0);
    let q_ref = b.get_arg(1);
    let tmp = b.build_load("tmp", b.ctx.i32_ty, p_ref);
    let qv  = b.build_load("qv",  b.ctx.i32_ty, q_ref);
    b.build_store(qv,  p_ref);
    b.build_store(tmp, q_ref);
    b.build_ret_void();

    let p = Printer::new(b.ctx);
    let ir = p.print_module(b.module);

    assert!(ir.contains("load i32"), "missing load:\n{}", ir);
    assert!(ir.contains("store i32"), "missing store:\n{}", ir);
    assert!(ir.contains("ret void"), "missing ret void:\n{}", ir);
}

/// Build a function with a global variable and verify the module output.
#[test]
fn roundtrip_global() {
    let mut ctx = Context::new();
    let mut module = Module::new("globals");
    module.target_triple = Some("x86_64-unknown-linux-gnu".to_string());
    let mut b = Builder::new(&mut ctx, &mut module);

    let init = b.ctx.const_int(b.ctx.i32_ty, 100);
    let gid = b.add_global("LIMIT", b.ctx.i32_ty, Some(init), true, Linkage::Internal);

    let p = Printer::new(b.ctx);
    let ir = p.print_module(b.module);

    assert!(ir.contains("target triple"), "missing triple:\n{}", ir);
    assert!(ir.contains("@LIMIT"), "missing global:\n{}", ir);
    assert!(ir.contains("internal"), "missing linkage:\n{}", ir);
    assert!(ir.contains("constant"), "missing constant keyword:\n{}", ir);
}
