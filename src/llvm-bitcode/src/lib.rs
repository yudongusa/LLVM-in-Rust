//! LLVM-in-Rust IR binary format (LRIR) reader and writer.
//!
//! This crate implements a compact binary serialization format for
//! `(Context, Module)` pairs, enabling round-trip fidelity without
//! depending on the full LLVM bitcode bitstream format.

pub mod error;
pub mod reader;
pub mod writer;

pub use error::BitcodeError;
pub use reader::read_bitcode;
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
}
