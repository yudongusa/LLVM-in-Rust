//! MIR place projection lowering.
//!
//! Translates rustc `Place` + `[PlaceElem]` chains into llvm-ir GEP + load sequences.
//!
//! # Stable / nightly split
//!
//! The core lowering logic (`lower_projection`) operates on the mock types defined here
//! and is fully usable on stable Rust.  Under `#[cfg(feature = "rustc-backend")]` an
//! adapter that maps rustc `PlaceElem` to our `PlaceElem` will be added once the
//! nightly interface is available.

// ── Stable-mode types (mock MIR for testing) ─────────────────────────────────

/// Simplified PlaceElem for testing without rustc_private.
///
/// Each variant models one step in a rustc `Place` projection chain.
#[derive(Debug, Clone)]
pub enum PlaceElem {
    /// Dereference a pointer.  Maps to a `load` instruction — the pointer
    /// value becomes the new base and the current element type advances to
    /// the pointee type.
    Deref,
    /// Access the `idx`-th field of a struct or tuple.  Maps to a
    /// `getelementptr inbounds` with indices `[0, idx]`.
    Field(usize),
    /// Dynamic array index (the index value is carried as a `ValueRef`
    /// handle inside the owning `IndexOperand`).  Use `PlaceElem::Index`
    /// together with `IndexOperand` to pass the runtime index value.
    Index,
    /// Cast to a specific enum variant.  Maps to two GEPs:
    /// 1. `[0, 1]` — skip the discriminant field and reach the data union.
    /// 2. `[0, variant_idx]` — select the specific variant within the union.
    Downcast(usize),
    /// Constant index into an array or slice.
    ConstantIndex(u64),
    /// Sub-slice `[from .. to]`.  Maps to a GEP of `[from]`; the caller is
    /// responsible for adjusting the fat-pointer length to `to - from`.
    Subslice {
        /// Start index of the sub-slice.
        from: u64,
        /// Exclusive end index of the sub-slice.
        to: u64,
    },
}

/// Simplified place representation for testing.
#[derive(Debug, Clone)]
pub struct MockPlace {
    /// Index of the local variable that this place is rooted at.
    pub local: usize,
    /// Ordered chain of projections applied left-to-right.
    pub projection: Vec<PlaceElem>,
}

/// Carries the dynamic index value for an `Index` projection.
///
/// Because `PlaceElem::Index` cannot embed a `ValueRef` (it needs to stay
/// `Clone` without depending on a specific IR library version in its variant
/// payload), callers pass the operands in a parallel `Vec<Option<ValueRef>>`
/// that aligns with the projection list.  Each `Some` entry corresponds to an
/// `Index` projection at the same position.
pub use llvm_ir::context::ValueRef;

// ── Core lowering function ────────────────────────────────────────────────────

use llvm_ir::{Builder, context::TypeId, types::TypeData};

/// Lower a chain of MIR place projections into llvm-ir GEP / load instructions.
///
/// # Parameters
///
/// * `builder` — mutable IR builder positioned at the insertion point.
/// * `base_ptr` — `ValueRef` pointing to the root local (typically the result
///   of an `alloca`).
/// * `elem_ty` — the element type that `base_ptr` currently points to.
/// * `projections` — ordered slice of `PlaceElem` steps to apply.
/// * `index_operands` — parallel slice of dynamic index values.  Element `i`
///   must be `Some(vref)` when `projections[i]` is `PlaceElem::Index`, and
///   `None` otherwise.
///
/// # Returns
///
/// `(ptr, ty)` where `ptr` is a pointer to the projected location and `ty` is
/// the element type at that location.
pub fn lower_projection(
    builder: &mut Builder<'_>,
    base_ptr: ValueRef,
    elem_ty: TypeId,
    projections: &[PlaceElem],
    index_operands: &[Option<ValueRef>],
) -> (ValueRef, TypeId) {
    let mut ptr = base_ptr;
    let mut ty = elem_ty;
    let mut index_slot = 0usize;

    for (proj_idx, proj) in projections.iter().enumerate() {
        match proj {
            // ── Deref ──────────────────────────────────────────────────────
            // The current `ptr` is a pointer-to-pointer.  Emit a load to
            // obtain the inner pointer, then continue with the pointee type.
            PlaceElem::Deref => {
                let ptr_ty = builder.ctx.ptr_ty;
                let loaded = builder.build_load(
                    format!("deref_{proj_idx}"),
                    ptr_ty,
                    ptr,
                );
                // After a deref the element type is the inner pointee.
                // We represent all pointers as opaque `ptr` in this IR, so
                // we keep `ty` unchanged (the caller already knows the
                // pointee type from the MIR type system).
                ptr = loaded;
                // ty remains the same — the caller's type annotation drives it.
            }

            // ── Field ───────────────────────────────────────────────────────
            // `ptr` points to a struct.  Emit `getelementptr inbounds ptr,
            // i32 0, i32 <idx>` and advance `ty` to the field type.
            PlaceElem::Field(idx) => {
                let i32_ty = builder.ctx.i32_ty;
                let zero = builder.const_int(i32_ty, 0);
                let field_idx_val = builder.const_int(i32_ty, *idx as u64);
                let gep = builder.build_gep_inbounds(
                    format!("field_{proj_idx}_{idx}"),
                    ty,
                    ptr,
                    vec![zero, field_idx_val],
                );
                // Advance ty to the field type if the current type is a struct.
                ty = field_type(builder, ty, *idx);
                ptr = gep;
            }

            // ── Index (dynamic) ─────────────────────────────────────────────
            // `ptr` points to an array.  Emit `getelementptr inbounds ptr,
            // i64 <dynamic_idx>`.  The dynamic index is taken from
            // `index_operands`.
            PlaceElem::Index => {
                let idx_val = index_operands
                    .get(index_slot)
                    .and_then(|o| *o)
                    .expect("Index projection requires a corresponding index_operand entry");
                index_slot += 1;

                let gep = builder.build_gep_inbounds(
                    format!("index_{proj_idx}"),
                    ty,
                    ptr,
                    vec![idx_val],
                );
                // Advance ty to the array element type.
                ty = array_element_type(builder, ty);
                ptr = gep;
            }

            // ── Downcast ────────────────────────────────────────────────────
            // The struct is modelled as `{ discriminant, data_union }`.
            // First GEP to field 1 (the data union), then GEP to field
            // `variant_idx` inside it.
            PlaceElem::Downcast(variant_idx) => {
                let i32_ty = builder.ctx.i32_ty;
                let zero = builder.const_int(i32_ty, 0);
                let one = builder.const_int(i32_ty, 1);
                // Step 1: reach the data union (field 1 of the enum struct).
                let data_gep = builder.build_gep_inbounds(
                    format!("downcast_data_{proj_idx}"),
                    ty,
                    ptr,
                    vec![zero, one],
                );
                // The data union type is field 1 of the outer struct.
                let union_ty = field_type(builder, ty, 1);
                // Step 2: select the variant within the data union.
                let variant_idx_val = builder.const_int(i32_ty, *variant_idx as u64);
                let variant_gep = builder.build_gep_inbounds(
                    format!("downcast_variant_{proj_idx}_{variant_idx}"),
                    union_ty,
                    data_gep,
                    vec![zero, variant_idx_val],
                );
                // Advance ty to the variant field type.
                ty = field_type(builder, union_ty, *variant_idx);
                ptr = variant_gep;
            }

            // ── ConstantIndex ───────────────────────────────────────────────
            // Emit `getelementptr inbounds ptr, i64 <offset>`.
            PlaceElem::ConstantIndex(offset) => {
                let i64_ty = builder.ctx.i64_ty;
                let offset_val = builder.const_int(i64_ty, *offset);
                let gep = builder.build_gep_inbounds(
                    format!("cidx_{proj_idx}_{offset}"),
                    ty,
                    ptr,
                    vec![offset_val],
                );
                // Advance ty to the array element type.
                ty = array_element_type(builder, ty);
                ptr = gep;
            }

            // ── Subslice ────────────────────────────────────────────────────
            // Emit `getelementptr inbounds ptr, i64 <from>`.
            // The caller is responsible for adjusting the fat-pointer length
            // field to `to - from` using the returned metadata.
            PlaceElem::Subslice { from, to: _ } => {
                let i64_ty = builder.ctx.i64_ty;
                let from_val = builder.const_int(i64_ty, *from);
                let gep = builder.build_gep_inbounds(
                    format!("subslice_{proj_idx}_{from}"),
                    ty,
                    ptr,
                    vec![from_val],
                );
                // ty stays the same (still pointing to the element type).
                ty = array_element_type(builder, ty);
                ptr = gep;
            }
        }
    }

    (ptr, ty)
}

// ── Private helpers ───────────────────────────────────────────────────────────

/// Return the type of the `idx`-th field of a struct type, or `ptr_ty` as a
/// conservative fallback for opaque / non-struct types.
fn field_type(builder: &Builder<'_>, struct_ty: TypeId, idx: usize) -> TypeId {
    match builder.ctx.get_type(struct_ty) {
        TypeData::Struct(st) => {
            st.fields.get(idx).copied().unwrap_or(builder.ctx.ptr_ty)
        }
        _ => builder.ctx.ptr_ty,
    }
}

/// Return the element type of an array type, or `ptr_ty` as a conservative
/// fallback for non-array types.
fn array_element_type(builder: &Builder<'_>, array_ty: TypeId) -> TypeId {
    match builder.ctx.get_type(array_ty) {
        TypeData::Array { element, .. } => *element,
        _ => builder.ctx.ptr_ty,
    }
}

// ── Nightly wiring (rustc_private) ───────────────────────────────────────────

#[cfg(feature = "rustc-backend")]
pub mod rustc_adapter {
    //! Adapter layer that maps `rustc_middle::mir::PlaceElem` to our `PlaceElem`.
    //!
    //! This module requires nightly + rustc-dev; it is excluded from stable
    //! CI.  The function signatures are kept intentionally coarse-grained so
    //! the adapter can be fleshed out incrementally.

    // TODO: uncomment once `#![feature(rustc_private)]` is active.
    // extern crate rustc_middle;
    //
    // use rustc_middle::mir::PlaceElem as RustcElem;
    // use super::PlaceElem;
    //
    // /// Translate a rustc `PlaceElem` to our simplified `PlaceElem`.
    // ///
    // /// `ty_bits` is the bit-width of the projection type (used for
    // /// `ConstantIndex`'s `offset` field).
    // pub fn from_rustc<'tcx>(elem: &RustcElem<'tcx>) -> Option<PlaceElem> {
    //     match elem {
    //         RustcElem::Deref => Some(PlaceElem::Deref),
    //         RustcElem::Field(f, _ty) => Some(PlaceElem::Field(f.index())),
    //         RustcElem::Index(_) => Some(PlaceElem::Index),
    //         RustcElem::Downcast(_, idx) => Some(PlaceElem::Downcast(idx.as_usize())),
    //         RustcElem::ConstantIndex { offset, .. } => Some(PlaceElem::ConstantIndex(*offset as u64)),
    //         RustcElem::Subslice { from, to, .. } => Some(PlaceElem::Subslice {
    //             from: *from as u64,
    //             to: *to as u64,
    //         }),
    //         _ => None,
    //     }
    // }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use llvm_ir::{
        Builder, Context, Module,
        context::ValueRef,
        instruction::InstrKind,
        value::Linkage,
    };

    // ── Test 1: Deref ─────────────────────────────────────────────────────────

    /// A single `Deref` on a pointer emits a `load` instruction and the result
    /// pointer is the return value of `lower_projection`.
    #[test]
    fn deref_projection_emits_load() {
        let mut ctx = Context::new();
        let mut module = Module::new("test");
        let i32_ty = ctx.i32_ty;
        let ptr_ty = ctx.ptr_ty;
        let void_ty = ctx.void_ty;

        let mut b = Builder::new(&mut ctx, &mut module);
        let fid = b.add_function("test_deref", void_ty, vec![ptr_ty], vec!["p".into()], false, Linkage::Internal);
        let entry = b.add_block("entry");
        b.position_at_end(entry);

        // The alloca pointer (argument %p acts as the base pointer to a *i32).
        let base_ptr = b.get_arg(0);

        let projections = vec![PlaceElem::Deref];
        let (result_ptr, _result_ty) = lower_projection(&mut b, base_ptr, i32_ty, &projections, &[None]);

        // The result must be a new instruction (load).
        assert!(
            matches!(result_ptr, ValueRef::Instruction(_)),
            "Deref must produce an instruction ValueRef"
        );

        // Inspect the emitted instruction.
        let func = b.module.function(fid);
        let instrs: Vec<_> = func.blocks.iter().flat_map(|bb| bb.body.iter().copied()).collect();
        assert!(!instrs.is_empty(), "at least one instruction must be emitted");
        let last_instr = func.instr(*instrs.last().unwrap());
        assert!(
            matches!(last_instr.kind, InstrKind::Load { .. }),
            "final instruction must be Load, got {:?}",
            last_instr.kind
        );

        b.build_ret_void();
    }

    // ── Test 2: Field ─────────────────────────────────────────────────────────

    /// `Field(1)` on a `{i32, i64}` struct emits a GEP with indices `[0, 1]`.
    #[test]
    fn field_projection_emits_gep() {
        let mut ctx = Context::new();
        let mut module = Module::new("test");
        let i32_ty = ctx.i32_ty;
        let i64_ty = ctx.i64_ty;
        let ptr_ty = ctx.ptr_ty;
        let void_ty = ctx.void_ty;
        let struct_ty = ctx.mk_struct_anon(vec![i32_ty, i64_ty], false);

        let mut b = Builder::new(&mut ctx, &mut module);
        let fid = b.add_function("test_field", void_ty, vec![ptr_ty], vec!["p".into()], false, Linkage::Internal);
        let entry = b.add_block("entry");
        b.position_at_end(entry);

        let base_ptr = b.get_arg(0);

        let projections = vec![PlaceElem::Field(1)];
        let (result_ptr, result_ty) = lower_projection(&mut b, base_ptr, struct_ty, &projections, &[]);

        assert!(
            matches!(result_ptr, ValueRef::Instruction(_)),
            "Field must produce an instruction"
        );

        // The result type must be i64 (field 1 of {i32, i64}).
        assert_eq!(result_ty, i64_ty, "Field(1) result type must be i64");

        let func = b.module.function(fid);
        let instrs: Vec<_> = func.blocks.iter().flat_map(|bb| bb.body.iter().copied()).collect();
        let last_instr = func.instr(*instrs.last().unwrap());
        match &last_instr.kind {
            InstrKind::GetElementPtr { inbounds, indices, .. } => {
                assert!(inbounds, "GEP must be inbounds");
                assert_eq!(indices.len(), 2, "Field GEP must have 2 indices");
            }
            other => panic!("Expected GEP, got {:?}", other),
        }

        b.build_ret_void();
    }

    // ── Test 3: Index (dynamic) ───────────────────────────────────────────────

    /// `Index` on `[4 x i32]` emits a GEP with the dynamic index value.
    #[test]
    fn index_projection_emits_gep() {
        let mut ctx = Context::new();
        let mut module = Module::new("test");
        let i32_ty = ctx.i32_ty;
        let i64_ty = ctx.i64_ty;
        let ptr_ty = ctx.ptr_ty;
        let void_ty = ctx.void_ty;
        let arr_ty = ctx.mk_array(i32_ty, 4);

        let mut b = Builder::new(&mut ctx, &mut module);
        let fid = b.add_function(
            "test_index",
            void_ty,
            vec![ptr_ty, i64_ty],
            vec!["arr".into(), "idx".into()],
            false,
            Linkage::Internal,
        );
        let entry = b.add_block("entry");
        b.position_at_end(entry);

        let base_ptr = b.get_arg(0);
        let dyn_idx = b.get_arg(1); // i64 index

        let projections = vec![PlaceElem::Index];
        let index_operands = vec![Some(dyn_idx)];
        let (result_ptr, result_ty) =
            lower_projection(&mut b, base_ptr, arr_ty, &projections, &index_operands);

        assert!(
            matches!(result_ptr, ValueRef::Instruction(_)),
            "Index must produce an instruction"
        );
        // Element type of [4 x i32] is i32.
        assert_eq!(result_ty, i32_ty, "Index result type must be i32");

        let func = b.module.function(fid);
        let instrs: Vec<_> = func.blocks.iter().flat_map(|bb| bb.body.iter().copied()).collect();
        let last_instr = func.instr(*instrs.last().unwrap());
        match &last_instr.kind {
            InstrKind::GetElementPtr { inbounds, indices, .. } => {
                assert!(inbounds, "GEP must be inbounds");
                assert_eq!(indices.len(), 1, "Index GEP must have 1 index");
                // The one index must be the dynamic argument.
                assert_eq!(indices[0], dyn_idx);
            }
            other => panic!("Expected GEP, got {:?}", other),
        }

        b.build_ret_void();
    }

    // ── Test 4: ConstantIndex ─────────────────────────────────────────────────

    /// `ConstantIndex(2)` emits a GEP with constant index 2.
    #[test]
    fn constant_index_projection_emits_gep() {
        let mut ctx = Context::new();
        let mut module = Module::new("test");
        let i32_ty = ctx.i32_ty;
        let ptr_ty = ctx.ptr_ty;
        let void_ty = ctx.void_ty;
        let arr_ty = ctx.mk_array(i32_ty, 8);

        let mut b = Builder::new(&mut ctx, &mut module);
        let fid = b.add_function(
            "test_cidx",
            void_ty,
            vec![ptr_ty],
            vec!["arr".into()],
            false,
            Linkage::Internal,
        );
        let entry = b.add_block("entry");
        b.position_at_end(entry);

        let base_ptr = b.get_arg(0);

        let projections = vec![PlaceElem::ConstantIndex(2)];
        let (result_ptr, result_ty) =
            lower_projection(&mut b, base_ptr, arr_ty, &projections, &[]);

        assert!(
            matches!(result_ptr, ValueRef::Instruction(_)),
            "ConstantIndex must produce an instruction"
        );
        assert_eq!(result_ty, i32_ty, "ConstantIndex result type must be i32");

        let func = b.module.function(fid);
        let instrs: Vec<_> = func.blocks.iter().flat_map(|bb| bb.body.iter().copied()).collect();
        let last_instr = func.instr(*instrs.last().unwrap());
        match &last_instr.kind {
            InstrKind::GetElementPtr { inbounds, indices, .. } => {
                assert!(inbounds, "GEP must be inbounds");
                assert_eq!(indices.len(), 1, "ConstantIndex GEP must have 1 index");
            }
            other => panic!("Expected GEP, got {:?}", other),
        }

        b.build_ret_void();
    }

    // ── Test 5: Chained Deref then Field ──────────────────────────────────────

    /// `Deref` followed by `Field(0)` on a `*{i32, i64}` emits a load then a GEP.
    #[test]
    fn chained_projections_deref_then_field() {
        let mut ctx = Context::new();
        let mut module = Module::new("test");
        let i32_ty = ctx.i32_ty;
        let i64_ty = ctx.i64_ty;
        let ptr_ty = ctx.ptr_ty;
        let void_ty = ctx.void_ty;
        let struct_ty = ctx.mk_struct_anon(vec![i32_ty, i64_ty], false);

        let mut b = Builder::new(&mut ctx, &mut module);
        let fid = b.add_function(
            "test_deref_field",
            void_ty,
            vec![ptr_ty],
            vec!["pp".into()],
            false,
            Linkage::Internal,
        );
        let entry = b.add_block("entry");
        b.position_at_end(entry);

        let base_ptr = b.get_arg(0);

        // Project: Deref (ptr→ptr to struct), then Field(0) (first i32 field).
        let projections = vec![PlaceElem::Deref, PlaceElem::Field(0)];
        let (result_ptr, result_ty) =
            lower_projection(&mut b, base_ptr, struct_ty, &projections, &[None]);

        assert!(
            matches!(result_ptr, ValueRef::Instruction(_)),
            "chained projection must produce an instruction"
        );
        assert_eq!(result_ty, i32_ty, "Field(0) of {{i32,i64}} must be i32");

        // Expect exactly 2 instructions: load then GEP.
        let func = b.module.function(fid);
        let instrs: Vec<_> = func.blocks.iter().flat_map(|bb| bb.body.iter().copied()).collect();
        assert_eq!(instrs.len(), 2, "must emit exactly 2 instructions (load + gep)");
        assert!(
            matches!(func.instr(instrs[0]).kind, InstrKind::Load { .. }),
            "first instruction must be Load"
        );
        assert!(
            matches!(func.instr(instrs[1]).kind, InstrKind::GetElementPtr { .. }),
            "second instruction must be GEP"
        );

        b.build_ret_void();
    }

    // ── Test 6: Downcast ──────────────────────────────────────────────────────

    /// `Downcast(1)` on an enum `{i32, {i32, i64}}` emits two GEPs.
    #[test]
    fn downcast_projection_emits_gep() {
        let mut ctx = Context::new();
        let mut module = Module::new("test");
        let i32_ty = ctx.i32_ty;
        let i64_ty = ctx.i64_ty;
        let ptr_ty = ctx.ptr_ty;
        let void_ty = ctx.void_ty;

        // Model enum as { discriminant: i32, data: { variant0: i32, variant1: {i32,i64} } }
        let variant1_ty = ctx.mk_struct_anon(vec![i32_ty, i64_ty], false);
        let data_union_ty = ctx.mk_struct_anon(vec![i32_ty, variant1_ty], false);
        let enum_ty = ctx.mk_struct_anon(vec![i32_ty, data_union_ty], false);

        let mut b = Builder::new(&mut ctx, &mut module);
        let fid = b.add_function(
            "test_downcast",
            void_ty,
            vec![ptr_ty],
            vec!["e".into()],
            false,
            Linkage::Internal,
        );
        let entry = b.add_block("entry");
        b.position_at_end(entry);

        let base_ptr = b.get_arg(0);

        let projections = vec![PlaceElem::Downcast(1)];
        let (result_ptr, result_ty) =
            lower_projection(&mut b, base_ptr, enum_ty, &projections, &[]);

        assert!(
            matches!(result_ptr, ValueRef::Instruction(_)),
            "Downcast must produce an instruction"
        );

        // The result type must be the variant1 struct type.
        assert_eq!(
            result_ty, variant1_ty,
            "Downcast(1) result type must be variant1 struct"
        );

        // Two GEP instructions must have been emitted.
        let func = b.module.function(fid);
        let instrs: Vec<_> = func.blocks.iter().flat_map(|bb| bb.body.iter().copied()).collect();
        assert_eq!(instrs.len(), 2, "Downcast must emit exactly 2 GEP instructions");
        for &id in &instrs {
            assert!(
                matches!(func.instr(id).kind, InstrKind::GetElementPtr { inbounds: true, .. }),
                "both instructions must be inbounds GEPs"
            );
        }

        b.build_ret_void();
    }
}
