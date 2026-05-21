//! Aggregate type lowering for the rustc codegen backend.
//!
//! Translates MIR `Rvalue::Aggregate` into llvm-ir alloca + GEP + store sequences.
//!
//! Each aggregate kind maps to a canonical pattern:
//! - **Struct / Tuple** — `alloca {T0, T1, ...}` + per-field GEP + store
//! - **Array**          — `alloca [N x T]` + per-element GEP + store
//! - **Enum**           — `alloca {i8, [...]}` + discriminant store at field 0
//!   + variant data store at field 1..
//! - **Slice**          — fat-pointer pair `(ptr, len)`; the pointer comes from
//!   the first operand (no alloca emitted)

use llvm_ir::{
    basic_block::BasicBlock,
    context::{BlockId, Context, ValueRef},
    function::Function,
    instruction::{InstrKind, Instruction},
    TypeId,
};

// ── Public types ─────────────────────────────────────────────────────────────

/// Aggregate kind (mirrors `rustc_middle::mir::AggregateKind`).
#[derive(Debug, Clone)]
pub enum AggKind {
    /// Named struct with typed fields at layout offsets.
    Struct,
    /// Anonymous struct / tuple — same lowering as `Struct`.
    Tuple,
    /// Discriminated union: store the discriminant (i8) at field 0 of the IR
    /// struct, variant payload fields at indices 1..N.
    Enum {
        /// Index of the active variant (informational; not encoded separately).
        variant_idx: usize,
        /// Discriminant value to store at offset 0.
        discriminant: i64,
    },
    /// Fixed-length array `[N x T]`.
    Array,
    /// Slice fat-pointer: `operands[0]` is the backing `ptr`, `operands[1]` is
    /// the `len`.  Both are passed through as-is; no alloca is emitted.
    Slice,
}

/// A single aggregate operand (field / element value).
#[derive(Debug, Clone)]
pub struct AggOperand {
    /// The SSA value for this field/element.
    pub val: ValueRef,
    /// The IR type of `val`.
    pub ty: TypeId,
}

// ── Main entry point ─────────────────────────────────────────────────────────

/// Lower an aggregate construction into IR instructions.
///
/// Emits an `alloca result_ty` into **block 0** (the function entry block) so
/// that it dominates all uses, then emits `getelementptr inbounds` + `store`
/// pairs into `block` for every field / element.
///
/// For [`AggKind::Enum`], the discriminant is stored first (field index 0),
/// followed by each variant operand at indices 1, 2, …
///
/// For [`AggKind::Slice`], no alloca is emitted; the first operand is returned
/// directly as the fat-pointer base.
///
/// # Panics
/// Panics if `func.blocks` is empty (i.e., the function has no entry block).
///
/// # Returns
/// The `ValueRef::Instruction` of the alloca (or the raw pointer for slices).
pub fn lower_aggregate(
    ctx: &mut Context,
    func: &mut Function,
    block: BlockId,
    kind: &AggKind,
    operands: &[AggOperand],
    result_ty: TypeId,
) -> ValueRef {
    // ── Slice short-circuit ───────────────────────────────────────────────────
    if let AggKind::Slice = kind {
        return operands
            .first()
            .map(|op| op.val)
            .unwrap_or_else(|| ValueRef::Constant(ctx.const_null(ctx.ptr_ty)));
    }

    // ── Emit alloca into entry block (block 0) ────────────────────────────────
    let ptr_ty = ctx.ptr_ty;

    // If the function somehow has no blocks yet, create the entry block.
    let entry_bid = if func.blocks.is_empty() {
        let bb = BasicBlock::new("entry");
        func.add_block(bb)
    } else {
        BlockId(0)
    };

    let alloca_name = func.fresh_name();
    let alloca_instr = Instruction::new(
        Some(alloca_name),
        ptr_ty,
        InstrKind::Alloca {
            alloc_ty: result_ty,
            num_elements: None,
            align: None,
        },
    );
    let alloca_id = func.alloc_instr(alloca_instr);
    // Insert at the front of the entry block so it dominates all uses.
    func.block_mut(entry_bid).body.insert(0, alloca_id);
    let alloca_ref = ValueRef::Instruction(alloca_id);

    // ── Common helpers ────────────────────────────────────────────────────────
    let i32_ty = ctx.i32_ty;
    let zero = ValueRef::Constant(ctx.const_int(i32_ty, 0));

    /// Emit a GEP + store pair for one field.
    macro_rules! emit_gep_store {
        ($field_const_idx:expr, $val:expr) => {{
            let field_idx = ValueRef::Constant(ctx.const_int(i32_ty, $field_const_idx));
            let gep_name = func.fresh_name();
            let gep_id = func.alloc_instr(Instruction::new(
                Some(gep_name),
                ptr_ty,
                InstrKind::GetElementPtr {
                    inbounds: true,
                    base_ty: result_ty,
                    ptr: alloca_ref,
                    indices: vec![zero, field_idx],
                },
            ));
            func.block_mut(block).append_instr(gep_id);

            let void_ty = ctx.void_ty;
            let store_id = func.alloc_instr(Instruction::new(
                None,
                void_ty,
                InstrKind::Store {
                    val: $val,
                    ptr: ValueRef::Instruction(gep_id),
                    align: None,
                    volatile: false,
                },
            ));
            func.block_mut(block).append_instr(store_id);
        }};
    }

    // ── Emit per-kind field stores ────────────────────────────────────────────
    match kind {
        AggKind::Struct | AggKind::Tuple => {
            for (idx, op) in operands.iter().enumerate() {
                emit_gep_store!(idx as u64, op.val);
            }
        }

        AggKind::Array => {
            for (idx, op) in operands.iter().enumerate() {
                emit_gep_store!(idx as u64, op.val);
            }
        }

        AggKind::Enum { discriminant, .. } => {
            // Field 0: discriminant (i8).
            let i8_ty = ctx.i8_ty;
            let disc_val = ValueRef::Constant(ctx.const_int(i8_ty, *discriminant as u64));
            emit_gep_store!(0u64, disc_val);

            // Fields 1..: variant payload operands.
            for (idx, op) in operands.iter().enumerate() {
                emit_gep_store!((idx + 1) as u64, op.val);
            }
        }

        // Already handled at the top.
        AggKind::Slice => unreachable!(),
    }

    alloca_ref
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use llvm_ir::{
        basic_block::BasicBlock,
        context::{BlockId, Context, ValueRef},
        function::Function,
        instruction::InstrKind,
        value::{ConstantData, Linkage},
    };

    // ─── helpers ──────────────────────────────────────────────────────────────

    /// Build a minimal function with a single entry block.
    fn make_func(ctx: &mut Context) -> (Function, BlockId) {
        let void_ty = ctx.void_ty;
        let fn_ty = ctx.mk_fn_type(void_ty, vec![], false);
        let mut func = Function::new("test_agg", fn_ty, vec![], Linkage::Internal);
        let bb = BasicBlock::new("entry");
        let bid = func.add_block(bb);
        (func, bid)
    }

    /// Count instructions matching a predicate.
    fn count_instrs<F>(func: &Function, pred: F) -> usize
    where
        F: Fn(&InstrKind) -> bool,
    {
        func.instructions.iter().filter(|i| pred(&i.kind)).count()
    }

    fn is_alloca(k: &InstrKind) -> bool {
        matches!(k, InstrKind::Alloca { .. })
    }
    fn is_gep(k: &InstrKind) -> bool {
        matches!(k, InstrKind::GetElementPtr { .. })
    }
    fn is_store(k: &InstrKind) -> bool {
        matches!(k, InstrKind::Store { .. })
    }

    // ─── 1. struct_aggregate_emits_alloca_and_stores ─────────────────────────

    #[test]
    fn struct_aggregate_emits_alloca_and_stores() {
        let mut ctx = Context::new();
        let (mut func, entry_bid) = make_func(&mut ctx);

        // {i32, i64}
        let i32_ty = ctx.i32_ty;
        let i64_ty = ctx.i64_ty;
        let struct_ty = ctx.mk_struct_anon(vec![i32_ty, i64_ty], false);

        let val1 = ValueRef::Constant(ctx.const_int(i32_ty, 1));
        let val2 = ValueRef::Constant(ctx.const_int(i64_ty, 2));

        let operands = vec![
            AggOperand { val: val1, ty: i32_ty },
            AggOperand { val: val2, ty: i64_ty },
        ];

        let result = lower_aggregate(
            &mut ctx,
            &mut func,
            entry_bid,
            &AggKind::Struct,
            &operands,
            struct_ty,
        );

        assert!(matches!(result, ValueRef::Instruction(_)));
        assert_eq!(count_instrs(&func, is_alloca), 1, "exactly one alloca");
        assert_eq!(count_instrs(&func, is_gep), 2, "one GEP per field");
        assert_eq!(count_instrs(&func, is_store), 2, "one store per field");
    }

    // ─── 2. tuple_aggregate_same_as_struct ───────────────────────────────────

    #[test]
    fn tuple_aggregate_same_as_struct() {
        let mut ctx = Context::new();
        let (mut func, entry_bid) = make_func(&mut ctx);

        let i32_ty = ctx.i32_ty;
        let tuple_ty = ctx.mk_struct_anon(vec![i32_ty, i32_ty, i32_ty], false);

        let operands: Vec<AggOperand> = [10u64, 20, 30]
            .iter()
            .map(|&v| AggOperand {
                val: ValueRef::Constant(ctx.const_int(i32_ty, v)),
                ty: i32_ty,
            })
            .collect();

        let result = lower_aggregate(
            &mut ctx,
            &mut func,
            entry_bid,
            &AggKind::Tuple,
            &operands,
            tuple_ty,
        );

        assert!(matches!(result, ValueRef::Instruction(_)));
        assert_eq!(count_instrs(&func, is_alloca), 1);
        assert_eq!(count_instrs(&func, is_gep), 3);
        assert_eq!(count_instrs(&func, is_store), 3);
    }

    // ─── 3. array_aggregate_emits_element_stores ─────────────────────────────

    #[test]
    fn array_aggregate_emits_element_stores() {
        let mut ctx = Context::new();
        let (mut func, entry_bid) = make_func(&mut ctx);

        let i32_ty = ctx.i32_ty;
        let arr_ty = ctx.mk_array(i32_ty, 3);

        let operands: Vec<AggOperand> = [1u64, 2, 3]
            .iter()
            .map(|&v| AggOperand {
                val: ValueRef::Constant(ctx.const_int(i32_ty, v)),
                ty: i32_ty,
            })
            .collect();

        let result = lower_aggregate(
            &mut ctx,
            &mut func,
            entry_bid,
            &AggKind::Array,
            &operands,
            arr_ty,
        );

        assert!(matches!(result, ValueRef::Instruction(_)));
        assert_eq!(count_instrs(&func, is_alloca), 1);
        assert_eq!(count_instrs(&func, is_gep), 3, "one GEP per element");
        assert_eq!(count_instrs(&func, is_store), 3, "one store per element");
    }

    // ─── 4. enum_aggregate_stores_discriminant ───────────────────────────────

    #[test]
    fn enum_aggregate_stores_discriminant() {
        let mut ctx = Context::new();
        let (mut func, entry_bid) = make_func(&mut ctx);

        // Enum IR type: {i8, i32}
        let i8_ty = ctx.i8_ty;
        let i32_ty = ctx.i32_ty;
        let enum_ty = ctx.mk_struct_anon(vec![i8_ty, i32_ty], false);

        let data_val = ValueRef::Constant(ctx.const_int(i32_ty, 42));
        let operands = vec![AggOperand { val: data_val, ty: i32_ty }];

        lower_aggregate(
            &mut ctx,
            &mut func,
            entry_bid,
            &AggKind::Enum { variant_idx: 1, discriminant: 1 },
            &operands,
            enum_ty,
        );

        // 1 alloca + 2 GEPs (discriminant + data) + 2 stores
        assert_eq!(count_instrs(&func, is_alloca), 1);
        assert_eq!(count_instrs(&func, is_gep), 2);
        assert_eq!(count_instrs(&func, is_store), 2);

        // The first store must store an i8 constant with value 1.
        let stores: Vec<&InstrKind> =
            func.instructions.iter().map(|i| &i.kind).filter(|k| is_store(k)).collect();
        if let InstrKind::Store { val, .. } = stores[0] {
            if let ValueRef::Constant(cid) = val {
                let cd = ctx.get_const(*cid);
                if let ConstantData::Int { ty, val } = cd {
                    assert_eq!(*ty, ctx.i8_ty, "discriminant must be i8");
                    assert_eq!(*val, 1u64, "discriminant value must be 1");
                } else {
                    panic!("discriminant must be ConstantData::Int, got: {:?}", cd);
                }
            } else {
                panic!("discriminant val must be a constant");
            }
        }
    }

    // ─── 5. enum_aggregate_stores_variant_data ───────────────────────────────

    #[test]
    fn enum_aggregate_stores_variant_data() {
        let mut ctx = Context::new();
        let (mut func, entry_bid) = make_func(&mut ctx);

        let i8_ty = ctx.i8_ty;
        let i32_ty = ctx.i32_ty;
        let enum_ty = ctx.mk_struct_anon(vec![i8_ty, i32_ty], false);

        let payload = ValueRef::Constant(ctx.const_int(i32_ty, 99));
        let operands = vec![AggOperand { val: payload, ty: i32_ty }];

        lower_aggregate(
            &mut ctx,
            &mut func,
            entry_bid,
            &AggKind::Enum { variant_idx: 0, discriminant: 0 },
            &operands,
            enum_ty,
        );

        // Two stores: discriminant + payload
        assert_eq!(count_instrs(&func, is_store), 2, "disc store + payload store");

        // The GEP for the payload field must use field index 1.
        let geps: Vec<&InstrKind> =
            func.instructions.iter().map(|i| &i.kind).filter(|k| is_gep(k)).collect();
        assert_eq!(geps.len(), 2, "disc GEP + payload GEP");

        if let InstrKind::GetElementPtr { indices, .. } = geps[1] {
            // indices[1] is the field selector; should be constant 1.
            if let ValueRef::Constant(cid) = indices[1] {
                if let ConstantData::Int { val, .. } = ctx.get_const(cid) {
                    assert_eq!(*val, 1u64, "variant data GEP index must be 1");
                }
            }
        }
    }

    // ─── 6. single_field_struct ───────────────────────────────────────────────

    #[test]
    fn single_field_struct() {
        let mut ctx = Context::new();
        let (mut func, entry_bid) = make_func(&mut ctx);

        let i64_ty = ctx.i64_ty;
        let struct_ty = ctx.mk_struct_anon(vec![i64_ty], false);

        let val = ValueRef::Constant(ctx.const_int(i64_ty, 7));
        let operands = vec![AggOperand { val, ty: i64_ty }];

        let result = lower_aggregate(
            &mut ctx,
            &mut func,
            entry_bid,
            &AggKind::Struct,
            &operands,
            struct_ty,
        );

        assert!(matches!(result, ValueRef::Instruction(_)));
        assert_eq!(count_instrs(&func, is_alloca), 1);
        assert_eq!(count_instrs(&func, is_gep), 1);
        assert_eq!(count_instrs(&func, is_store), 1);
    }

    // ─── 7. zero_field_struct (unit struct) ──────────────────────────────────

    #[test]
    fn zero_field_struct() {
        let mut ctx = Context::new();
        let (mut func, entry_bid) = make_func(&mut ctx);

        let unit_ty = ctx.mk_struct_anon(vec![], false);
        let operands: Vec<AggOperand> = vec![];

        let result = lower_aggregate(
            &mut ctx,
            &mut func,
            entry_bid,
            &AggKind::Struct,
            &operands,
            unit_ty,
        );

        assert!(matches!(result, ValueRef::Instruction(_)));
        // Only the alloca; no GEPs or stores for a zero-field struct.
        assert_eq!(count_instrs(&func, is_alloca), 1, "alloca for unit struct");
        assert_eq!(count_instrs(&func, is_gep), 0, "no GEPs for unit struct");
        assert_eq!(count_instrs(&func, is_store), 0, "no stores for unit struct");
    }
}
