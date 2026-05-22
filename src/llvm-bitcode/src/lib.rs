//! LLVM-in-Rust IR binary format (LRIR) reader and writer, plus a standard
//! LLVM bitcode (`.bc`) reader.
//!
//! This crate implements:
//! - A compact binary serialization format for `(Context, Module)` pairs
//!   (LRIR), enabling round-trip fidelity.
//! - A standard LLVM bitcode reader (`read_llvm_bc`) that can parse `.bc`
//!   files produced by `clang -emit-llvm -c`.
//!
//! # Examples
//!
//! Round-trip a module through bitcode:
//!
//! ```no_run
//! use llvm_ir::{Context, Module};
//! use llvm_bitcode::{read_bitcode, write_bitcode};
//! let ctx = Context::new();
//! let module = Module::new("test");
//! let bytes = write_bitcode(&ctx, &module);
//! let (_ctx2, _module2) = read_bitcode(&bytes).expect("round-trip failed");
//! ```

pub mod error;
/// Low-level LLVM bitstream decoder (VBR, blocks, abbreviations).
pub mod bitstream;
/// Standard LLVM `.bc` file reader.
pub mod llvm_reader;
/// Public API for `reader`.
pub mod reader;
/// Public API for `writer`.
pub mod writer;

/// Public API for `re-export`.
pub use error::BitcodeError;
/// Public API for `re-export`.
pub use llvm_reader::read_llvm_bc;
/// Public API for `re-export`.
pub use reader::read_bitcode;
/// Public API for `re-export`.
pub use writer::write_bitcode;

// ── tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use llvm_ir::{Builder, Context, Linkage, Module};

    fn make_empty_module() -> (Context, Module) {
        let ctx = Context::new();
        let module = Module::new("empty");
        (ctx, module)
    }

    fn make_add_fn() -> (Context, Module) {
        let mut ctx = Context::new();
        let mut module = Module::new("test");
        let mut b = Builder::new(&mut ctx, &mut module);
        b.add_function(
            "add",
            b.ctx.i64_ty,
            vec![b.ctx.i64_ty, b.ctx.i64_ty],
            vec!["a".into(), "b".into()],
            false,
            Linkage::External,
        );
        let entry = b.add_block("entry");
        b.position_at_end(entry);
        let a = b.get_arg(0);
        let bv = b.get_arg(1);
        let sum = b.build_add("sum", a, bv);
        b.build_ret(sum);
        (ctx, module)
    }

    #[test]
    fn write_then_read_empty_module() {
        let (ctx, module) = make_empty_module();
        let bytes = write_bitcode(&ctx, &module);
        let (ctx2, module2) = read_bitcode(&bytes).expect("round-trip must succeed");
        assert_eq!(module2.name, "empty");
        assert_eq!(module2.functions.len(), 0);
        // Context must have at minimum the built-in types.
        assert!(ctx2.num_types() > 0);
    }

    #[test]
    fn write_then_read_simple_function() {
        let (ctx, module) = make_add_fn();
        let bytes = write_bitcode(&ctx, &module);
        let (_, module2) = read_bitcode(&bytes).expect("round-trip must succeed");
        assert_eq!(module2.functions.len(), 1);
        let func = &module2.functions[0];
        // The function must have at least one block containing at least one instruction.
        assert!(!func.blocks.is_empty());
        assert!(!func.instructions.is_empty());
    }

    #[test]
    fn write_then_read_preserves_freeze_instruction() {
        let mut ctx = Context::new();
        let mut module = Module::new("freeze");
        let mut b = Builder::new(&mut ctx, &mut module);
        b.add_function(
            "freeze_id",
            b.ctx.i32_ty,
            vec![b.ctx.i32_ty],
            vec!["x".into()],
            false,
            Linkage::External,
        );
        let entry = b.add_block("entry");
        b.position_at_end(entry);
        let x = b.get_arg(0);
        let y = b.build_freeze("y", x);
        b.build_ret(y);

        let bytes = write_bitcode(&ctx, &module);
        let (_, module2) = read_bitcode(&bytes).expect("round-trip must succeed");
        let func = &module2.functions[0];
        let iid = func.blocks[0].body[0];
        assert_eq!(func.instr(iid).kind.opcode(), "freeze");
    }

    #[test]
    fn write_then_read_preserves_atomic_instructions() {
        use llvm_ir::{InstrKind, MemOrdering, RmwOp};
        let mut ctx = Context::new();
        let mut module = Module::new("atomics_bc");
        let mut b = Builder::new(&mut ctx, &mut module);
        b.add_function(
            "atomic_pipeline",
            b.ctx.void_ty,
            vec![b.ctx.ptr_ty, b.ctx.i32_ty, b.ctx.i32_ty],
            vec!["p".into(), "cmp".into(), "new".into()],
            false,
            Linkage::External,
        );
        let entry = b.add_block("entry");
        b.position_at_end(entry);
        let p = b.get_arg(0);
        let cmp = b.get_arg(1);
        let new_val = b.get_arg(2);
        let i32_ty = b.ctx.i32_ty;
        b.build_fence(MemOrdering::SeqCst);
        b.build_cmpxchg(
            "cas",
            i32_ty,
            p,
            cmp,
            new_val,
            MemOrdering::AcqRel,
            MemOrdering::Acquire,
            true,
            true,
        );
        b.build_atomicrmw(
            "old",
            RmwOp::Xchg,
            i32_ty,
            p,
            new_val,
            MemOrdering::Release,
            false,
        );
        b.build_ret_void();

        let bytes = write_bitcode(&ctx, &module);
        let (_, module2) = read_bitcode(&bytes).expect("round-trip must succeed");
        let func = &module2.functions[0];
        let bb = &func.blocks[0];
        assert_eq!(bb.body.len(), 3);

        match &func.instr(bb.body[0]).kind {
            InstrKind::Fence { ordering } => assert_eq!(*ordering, MemOrdering::SeqCst),
            other => panic!("expected Fence, got {other:?}"),
        }
        match &func.instr(bb.body[1]).kind {
            InstrKind::CmpXchg {
                success_ord,
                fail_ord,
                weak,
                volatile,
                ..
            } => {
                assert_eq!(*success_ord, MemOrdering::AcqRel);
                assert_eq!(*fail_ord, MemOrdering::Acquire);
                assert!(*weak);
                assert!(*volatile);
            }
            other => panic!("expected CmpXchg, got {other:?}"),
        }
        match &func.instr(bb.body[2]).kind {
            InstrKind::AtomicRmw {
                op,
                ordering,
                volatile,
                ..
            } => {
                assert_eq!(*op, RmwOp::Xchg);
                assert_eq!(*ordering, MemOrdering::Release);
                assert!(!*volatile);
            }
            other => panic!("expected AtomicRmw, got {other:?}"),
        }
    }

    #[test]
    fn write_then_read_preserves_function_names() {
        let (ctx, module) = make_add_fn();
        let bytes = write_bitcode(&ctx, &module);
        let (_, module2) = read_bitcode(&bytes).expect("round-trip must succeed");
        assert_eq!(module2.functions[0].name, "add");
    }

    #[test]
    fn write_then_read_multiple_functions() {
        let mut ctx = Context::new();
        let mut module = Module::new("multi");

        // Function 1: add.
        let mut b = Builder::new(&mut ctx, &mut module);
        b.add_function(
            "add",
            b.ctx.i64_ty,
            vec![b.ctx.i64_ty, b.ctx.i64_ty],
            vec!["x".into(), "y".into()],
            false,
            Linkage::External,
        );
        let entry1 = b.add_block("entry");
        b.position_at_end(entry1);
        let x = b.get_arg(0);
        let y = b.get_arg(1);
        let sum = b.build_add("sum", x, y);
        b.build_ret(sum);

        // Function 2: sub.
        b.add_function(
            "sub",
            b.ctx.i64_ty,
            vec![b.ctx.i64_ty, b.ctx.i64_ty],
            vec!["a".into(), "b".into()],
            false,
            Linkage::External,
        );
        let entry2 = b.add_block("entry");
        b.position_at_end(entry2);
        let a = b.get_arg(0);
        let bv = b.get_arg(1);
        let diff = b.build_sub("diff", a, bv);
        b.build_ret(diff);

        let bytes = write_bitcode(&ctx, &module);
        let (_, module2) = read_bitcode(&bytes).expect("round-trip must succeed");

        assert_eq!(module2.functions.len(), 2);
        assert_eq!(module2.functions[0].name, "add");
        assert_eq!(module2.functions[1].name, "sub");
    }

    #[test]
    fn metadata_type_round_trips_as_metadata_not_label() {
        // A Context that contains a Metadata type must deserialise back as
        // Metadata, not as Label (which was the previous incorrect fallback).
        use llvm_ir::TypeData;
        let mut ctx = Context::new();
        let meta_ty = ctx.mk_metadata();
        let module = Module::new("meta_test");
        let bytes = write_bitcode(&ctx, &module);
        let (ctx2, _) = read_bitcode(&bytes).expect("round-trip must succeed");
        // The serialised type at the same index must decode as Metadata.
        let td = ctx2.get_type(meta_ty);
        assert_eq!(
            td,
            &TypeData::Metadata,
            "Metadata type must round-trip as TypeData::Metadata, not Label"
        );
    }

    #[test]
    fn invalid_magic_returns_error() {
        let bad = b"BAAD\x01\x00\x00\x00\x00\x00\x00\x00";
        let result = read_bitcode(bad);
        assert!(result.is_err(), "invalid magic must return an error");
        if let Err(BitcodeError::InvalidMagic) = result { /* ok */
        } else {
            panic!("expected InvalidMagic error");
        }
    }

    // ── globals round-trip tests ───────────────────────────────────────────

    #[test]
    fn test_globals_round_trip_no_init() {
        use llvm_ir::GlobalVariable;
        let ctx = Context::new();
        let mut module = Module::new("test");
        module.add_global(GlobalVariable {
            name: "x".into(),
            ty: ctx.i32_ty,
            initializer: None,
            is_constant: false,
            linkage: Linkage::External,
        });
        let bytes = write_bitcode(&ctx, &module);
        let (ctx2, module2) = read_bitcode(&bytes).expect("round-trip failed");
        assert_eq!(module2.globals.len(), 1);
        let gv = &module2.globals[0];
        assert_eq!(gv.name, "x");
        assert_eq!(gv.ty, ctx2.i32_ty);
        assert!(!gv.is_constant);
        assert_eq!(gv.linkage, Linkage::External);
        assert!(gv.initializer.is_none());
    }

    #[test]
    fn test_globals_round_trip_with_int_init() {
        use llvm_ir::{ConstantData, GlobalVariable};
        let mut ctx = Context::new();
        let mut module = Module::new("test");
        let init = ctx.push_const(ConstantData::Int {
            ty: ctx.i32_ty,
            val: 42,
        });
        module.add_global(GlobalVariable {
            name: "answer".into(),
            ty: ctx.i32_ty,
            initializer: Some(init),
            is_constant: true,
            linkage: Linkage::Internal,
        });
        let bytes = write_bitcode(&ctx, &module);
        let (ctx2, module2) = read_bitcode(&bytes).expect("round-trip failed");
        assert_eq!(module2.globals.len(), 1);
        let gv = &module2.globals[0];
        assert_eq!(gv.name, "answer");
        assert_eq!(gv.ty, ctx2.i32_ty);
        assert!(gv.is_constant);
        assert_eq!(gv.linkage, Linkage::Internal);
        let init_cid = gv.initializer.expect("initializer must be present");
        match ctx2.get_const(init_cid) {
            ConstantData::Int { val, .. } => assert_eq!(*val, 42),
            other => panic!("expected Int constant, got {other:?}"),
        }
    }

    #[test]
    fn test_globals_round_trip_constant_array() {
        use llvm_ir::{ConstantData, GlobalVariable};
        let mut ctx = Context::new();
        let mut module = Module::new("test");
        // Build [i32 1, i32 2, i32 3].
        let e0 = ctx.push_const(ConstantData::Int { ty: ctx.i32_ty, val: 1 });
        let e1 = ctx.push_const(ConstantData::Int { ty: ctx.i32_ty, val: 2 });
        let e2 = ctx.push_const(ConstantData::Int { ty: ctx.i32_ty, val: 3 });
        let arr_ty = ctx.mk_array(ctx.i32_ty, 3);
        let init = ctx.push_const(ConstantData::Array {
            ty: arr_ty,
            elements: vec![e0, e1, e2],
        });
        module.add_global(GlobalVariable {
            name: "arr".into(),
            ty: arr_ty,
            initializer: Some(init),
            is_constant: true,
            linkage: Linkage::External,
        });
        let bytes = write_bitcode(&ctx, &module);
        let (ctx2, module2) = read_bitcode(&bytes).expect("round-trip failed");
        assert_eq!(module2.globals.len(), 1);
        let gv = &module2.globals[0];
        assert_eq!(gv.name, "arr");
        assert!(gv.is_constant);
        let init_cid = gv.initializer.expect("initializer must be present");
        match ctx2.get_const(init_cid) {
            ConstantData::Array { elements, .. } => {
                assert_eq!(elements.len(), 3);
                // Check element values survive.
                for (expected, &elem_cid) in [1u64, 2, 3].iter().zip(elements.iter()) {
                    match ctx2.get_const(elem_cid) {
                        ConstantData::Int { val, .. } => assert_eq!(*val, *expected),
                        other => panic!("expected Int, got {other:?}"),
                    }
                }
            }
            other => panic!("expected Array constant, got {other:?}"),
        }
    }

    #[test]
    fn test_globals_round_trip_preserves_linkage() {
        use llvm_ir::GlobalVariable;
        let ctx = Context::new();
        let mut module = Module::new("test");
        module.add_global(GlobalVariable {
            name: "sym".into(),
            ty: ctx.i32_ty,
            initializer: None,
            is_constant: false,
            linkage: Linkage::Internal,
        });
        let bytes = write_bitcode(&ctx, &module);
        let (_, module2) = read_bitcode(&bytes).expect("round-trip failed");
        assert_eq!(module2.globals.len(), 1);
        assert_eq!(module2.globals[0].linkage, Linkage::Internal);
        assert_eq!(module2.globals[0].name, "sym");
    }

    #[test]
    fn test_globals_round_trip_with_expr_init() {
        use llvm_ir::{ConstantData, ConstExprOp, GlobalId, GlobalVariable};
        let mut ctx = Context::new();
        let mut module = Module::new("test");
        // Declare an i8 array global first (GlobalId(0)).
        let i8_ty = ctx.mk_int(8);
        let arr_ty = ctx.mk_array(i8_ty, 4);
        let e0 = ctx.push_const(ConstantData::Int { ty: i8_ty, val: b'h' as u64 });
        let e1 = ctx.push_const(ConstantData::Int { ty: i8_ty, val: b'i' as u64 });
        let e2 = ctx.push_const(ConstantData::Int { ty: i8_ty, val: 0 });
        let e3 = ctx.push_const(ConstantData::Int { ty: i8_ty, val: 0 });
        let arr_init = ctx.push_const(ConstantData::Array {
            ty: arr_ty,
            elements: vec![e0, e1, e2, e3],
        });
        let gid0 = module.add_global(GlobalVariable {
            name: "str_data".into(),
            ty: arr_ty,
            initializer: Some(arr_init),
            is_constant: true,
            linkage: Linkage::Private,
        });
        // Now create a constexpr getelementptr pointing to GlobalId(0).
        let base_ptr = ctx.push_const(ConstantData::GlobalRef {
            ty: ctx.ptr_ty,
            id: gid0,
            name: "str_data".into(),
        });
        let idx0 = ctx.push_const(ConstantData::Int { ty: ctx.i64_ty, val: 0 });
        let gep_expr = ctx.push_const(ConstantData::Expr {
            ty: ctx.ptr_ty,
            op: ConstExprOp::GetElementPtr { inbounds: true, base_ty: arr_ty },
            operands: vec![base_ptr, idx0, idx0],
        });
        // Second global: a pointer initialized with the GEP expression.
        module.add_global(GlobalVariable {
            name: "str_ptr".into(),
            ty: ctx.ptr_ty,
            initializer: Some(gep_expr),
            is_constant: false,
            linkage: Linkage::External,
        });
        let bytes = write_bitcode(&ctx, &module);
        let (ctx2, module2) = read_bitcode(&bytes).expect("round-trip failed");
        assert_eq!(module2.globals.len(), 2);
        // Check GlobalRef survived.
        let str_ptr = &module2.globals[1];
        assert_eq!(str_ptr.name, "str_ptr");
        let expr_cid = str_ptr.initializer.expect("must have initializer");
        match ctx2.get_const(expr_cid) {
            ConstantData::Expr { op, operands, .. } => {
                assert!(
                    matches!(op, ConstExprOp::GetElementPtr { inbounds: true, .. }),
                    "must be inbounds GEP"
                );
                // First operand must be a GlobalRef pointing back to GlobalId(0).
                match ctx2.get_const(operands[0]) {
                    ConstantData::GlobalRef { id, name, .. } => {
                        assert_eq!(*id, GlobalId(0));
                        assert_eq!(name, "str_data");
                    }
                    other => panic!("expected GlobalRef, got {other:?}"),
                }
            }
            other => panic!("expected Expr constant, got {other:?}"),
        }
    }
}
