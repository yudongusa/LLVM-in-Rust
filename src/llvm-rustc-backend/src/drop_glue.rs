//! Drop glue emission for the rustc codegen backend.
//!
//! Emits `__drop_glue_<Type>` functions and cleanup basic blocks for
//! values going out of scope (normal and unwind paths).
//!
//! # What "drop glue" means
//!
//! When a Rust value goes out of scope the compiler must:
//! 1. Call `Drop::drop` on the value if it has a custom `Drop` impl.
//! 2. Recursively drop each field that itself needs dropping.
//!
//! In IR a drop-glue function looks like:
//!
//! ```text
//! define void @__drop_glue_MyStruct(ptr %self) {
//! entry:
//!   call void @_ZN8MyStruct4dropE(ptr %self)
//!   %fp = getelementptr inbounds %MyStruct, ptr %self, i32 0, i32 0
//!   call void @__drop_glue_FieldType(ptr %fp)
//!   ret void
//! }
//! ```
//!
//! For types with `NeedsDrop = false` (integers, floats, raw pointers, etc.)
//! no glue is emitted at all.

use llvm_ir::{
    BasicBlock, BlockId, Builder, Context, FunctionId, GlobalId, InstrKind, Instruction,
    Module, TailCallKind, TypeId, ValueRef,
};
use llvm_ir::value::Linkage;

// ---------------------------------------------------------------------------
// DropInfo
// ---------------------------------------------------------------------------

/// Description of a type's drop requirements (mirrors the NeedsDrop query
/// together with knowledge of whether a custom `Drop` impl exists).
#[derive(Debug, Clone)]
pub struct DropInfo {
    /// Type name, used to generate the glue function name
    /// (`__drop_glue_<type_name>`).
    pub type_name: String,
    /// `true` when the type has a hand-written `Drop::drop` implementation.
    pub has_custom_drop: bool,
    /// Indices of fields that themselves need dropping (recursive glue).
    pub droppable_fields: Vec<usize>,
}

/// Return `true` when the type requires any drop glue at all.
///
/// A type needs drop glue when it either has a custom `Drop` impl *or* has at
/// least one field that itself needs dropping.  Primitive types (integers,
/// floats, raw pointers, etc.) have neither and can be skipped entirely.
pub fn needs_drop(info: &DropInfo) -> bool {
    info.has_custom_drop || !info.droppable_fields.is_empty()
}

// ---------------------------------------------------------------------------
// emit_drop_glue
// ---------------------------------------------------------------------------

/// Emit a drop-glue function for the given type into `module`.
///
/// The generated function has the signature `void (ptr)` where the single
/// argument is a pointer to the value being dropped (`%self`).
///
/// # Arguments
///
/// * `ctx`             – the IR context (type interning + constant pool).
/// * `module`          – the module to add the function to.
/// * `info`            – drop requirements for the type.
/// * `self_ty`         – the IR struct type that represents the Rust type.
/// * `custom_drop_fn`  – if `info.has_custom_drop`, the `FunctionId` of the
///   hand-written `Drop::drop` function.
/// * `field_glue`      – `(field_index, glue_fn)` pairs for each droppable
///   field.  These are iterated in order, so callers
///   should pass them in reverse-declaration order when
///   they want Rust's "last field dropped first" semantics.
///
/// # Returns
///
/// `Some(FunctionId)` of the emitted glue function, or `None` if the type
/// does not need drop glue (`needs_drop(info) == false`).
pub fn emit_drop_glue(
    ctx: &mut Context,
    module: &mut Module,
    info: &DropInfo,
    self_ty: TypeId,
    custom_drop_fn: Option<FunctionId>,
    field_glue: &[(usize, FunctionId)],
) -> Option<FunctionId> {
    if !needs_drop(info) {
        return None;
    }

    let glue_name = format!("__drop_glue_{}", info.type_name);

    // Signature: void (ptr %self)
    let void_ty = ctx.void_ty;
    let ptr_ty = ctx.ptr_ty;

    let mut builder = Builder::new(ctx, module);

    let fid = builder.add_function(
        &glue_name,
        void_ty,
        vec![ptr_ty],
        vec!["self".to_string()],
        false,
        Linkage::External,
    );

    let entry = builder.add_block("entry");
    builder.position_at_end(entry);

    // Argument: %self (ptr)
    let self_arg = builder.get_arg(0);

    // 1. Call the custom Drop::drop impl, if present.
    if info.has_custom_drop {
        let custom_fid = custom_drop_fn
            .expect("has_custom_drop is true but no custom_drop_fn was provided");
        let fn_ty = builder.ctx.mk_fn_type(void_ty, vec![ptr_ty], false);
        // Functions are referenced via ValueRef::Global(GlobalId(fn_id.0)).
        let callee = ValueRef::Global(GlobalId(custom_fid.0));
        builder.build_call("", void_ty, fn_ty, callee, vec![self_arg]);
    }

    // 2. Recursively drop each droppable field.
    for &(field_idx, glue_fid) in field_glue {
        // GEP: ptr to field at index `field_idx` within the struct.
        let zero = builder.const_i32(0);
        let idx = builder.const_i32(field_idx as i32);
        let field_ptr = builder.build_gep_inbounds(
            "field_ptr",
            self_ty,
            self_arg,
            vec![zero, idx],
        );

        let fn_ty = builder.ctx.mk_fn_type(void_ty, vec![ptr_ty], false);
        let callee = ValueRef::Global(GlobalId(glue_fid.0));
        builder.build_call("", void_ty, fn_ty, callee, vec![field_ptr]);
    }

    // 3. Return void.
    builder.build_ret_void();

    Some(fid)
}

// ---------------------------------------------------------------------------
// emit_cleanup_block
// ---------------------------------------------------------------------------

/// Emit a cleanup basic block inside `func` that drops a single local variable
/// and then branches unconditionally to `next_block`.
///
/// This is the per-variable counterpart to [`emit_drop_glue`]: while drop-glue
/// functions are module-level utilities, cleanup blocks are inlined into the
/// function that owns the value.
///
/// # Arguments
///
/// * `ctx`        – the IR context.
/// * `func`       – the function to add the cleanup block to.
/// * `local_ptr`  – `ValueRef` of a pointer to the local variable to drop.
///   Typically the result of an `alloca` in the function entry block.
/// * `drop_fn`    – `FunctionId` of the drop-glue function to call.
/// * `next_block` – the block to branch to after the cleanup (e.g. the
///   successor that continues normal execution, or a landing pad for the unwind path).
///
/// # Returns
///
/// The `BlockId` of the newly emitted cleanup block.
pub fn emit_cleanup_block(
    ctx: &mut Context,
    func: &mut llvm_ir::Function,
    local_ptr: ValueRef,
    drop_fn: FunctionId,
    next_block: BlockId,
) -> BlockId {
    // Add cleanup block to the function.
    let bb = BasicBlock::new("cleanup");
    let cleanup_bid = func.add_block(bb);

    // Build the call instruction: call void @drop_fn(ptr %local_ptr)
    let void_ty = ctx.void_ty;
    let ptr_ty = ctx.ptr_ty;
    let fn_ty = ctx.mk_fn_type(void_ty, vec![ptr_ty], false);
    let callee = ValueRef::Global(GlobalId(drop_fn.0));

    let call_instr = Instruction::new(
        None,
        void_ty,
        InstrKind::Call {
            tail: TailCallKind::None,
            callee_ty: fn_ty,
            callee,
            args: vec![local_ptr],
        },
    );
    let call_id = func.alloc_instr(call_instr);
    func.block_mut(cleanup_bid).append_instr(call_id);

    // Terminator: unconditional branch to next_block.
    let br_instr = Instruction::new(
        None,
        void_ty,
        InstrKind::Br { dest: next_block },
    );
    let br_id = func.alloc_instr(br_instr);
    func.block_mut(cleanup_bid).set_terminator(br_id);

    cleanup_bid
}

// ---------------------------------------------------------------------------
// Tests (stable — no rustc_private)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use llvm_ir::{Context, Module};
    use llvm_ir::value::Linkage;

    // ------------------------------------------------------------------
    // Helpers
    // ------------------------------------------------------------------

    fn trivial_info() -> DropInfo {
        DropInfo {
            type_name: "Trivial".to_string(),
            has_custom_drop: false,
            droppable_fields: vec![],
        }
    }

    fn custom_drop_info() -> DropInfo {
        DropInfo {
            type_name: "MyType".to_string(),
            has_custom_drop: true,
            droppable_fields: vec![],
        }
    }

    fn droppable_fields_info() -> DropInfo {
        DropInfo {
            type_name: "Composite".to_string(),
            has_custom_drop: false,
            droppable_fields: vec![0],
        }
    }

    // ------------------------------------------------------------------
    // needs_drop tests
    // ------------------------------------------------------------------

    #[test]
    fn needs_drop_false_for_trivial_type() {
        assert!(!needs_drop(&trivial_info()));
    }

    #[test]
    fn needs_drop_true_for_custom_drop() {
        assert!(needs_drop(&custom_drop_info()));
    }

    #[test]
    fn needs_drop_true_for_droppable_fields() {
        assert!(needs_drop(&droppable_fields_info()));
    }

    // ------------------------------------------------------------------
    // emit_drop_glue tests
    // ------------------------------------------------------------------

    #[test]
    fn emit_drop_glue_returns_none_for_trivial() {
        let mut ctx = Context::new();
        let mut module = Module::new("test");
        let self_ty = ctx.i32_ty; // placeholder — doesn't matter for trivial type

        let result = emit_drop_glue(
            &mut ctx,
            &mut module,
            &trivial_info(),
            self_ty,
            None,
            &[],
        );

        assert!(result.is_none(), "trivial type must not emit drop glue");
    }

    #[test]
    fn emit_drop_glue_emits_function() {
        let mut ctx = Context::new();
        let mut module = Module::new("test");

        // Add a placeholder custom drop function to the module first.
        let void_ty = ctx.void_ty;
        let ptr_ty = ctx.ptr_ty;
        let fn_ty = ctx.mk_fn_type(void_ty, vec![ptr_ty], false);
        let custom_fn = llvm_ir::Function::new(
            "_ZN6MyType4dropE",
            fn_ty,
            vec![llvm_ir::value::Argument { name: "self".to_string(), ty: ptr_ty, index: 0 }],
            Linkage::External,
        );
        let custom_fid = module.add_function(custom_fn);

        // Use an anonymous struct type as the self type.
        let self_ty = ctx.mk_struct_anon(vec![ctx.i64_ty], false);

        let info = DropInfo {
            type_name: "MyType".to_string(),
            has_custom_drop: true,
            droppable_fields: vec![],
        };

        let result = emit_drop_glue(
            &mut ctx,
            &mut module,
            &info,
            self_ty,
            Some(custom_fid),
            &[],
        );

        assert!(result.is_some(), "must emit a function for a type with custom Drop");
        let glue_fid = result.unwrap();

        // Check the name.
        let func = module.function(glue_fid);
        assert_eq!(
            func.name, "__drop_glue_MyType",
            "glue function must be named __drop_glue_<type_name>"
        );
    }

    #[test]
    fn emit_drop_glue_calls_custom_drop() {
        let mut ctx = Context::new();
        let mut module = Module::new("test");

        // Add custom drop function first.
        let void_ty = ctx.void_ty;
        let ptr_ty = ctx.ptr_ty;
        let fn_ty = ctx.mk_fn_type(void_ty, vec![ptr_ty], false);
        let custom_fn = llvm_ir::Function::new(
            "_ZN6MyType4dropE",
            fn_ty,
            vec![llvm_ir::value::Argument { name: "self".to_string(), ty: ptr_ty, index: 0 }],
            Linkage::External,
        );
        let custom_fid = module.add_function(custom_fn);

        let self_ty = ctx.mk_struct_anon(vec![ctx.i64_ty], false);
        let info = DropInfo {
            type_name: "MyType".to_string(),
            has_custom_drop: true,
            droppable_fields: vec![],
        };

        let glue_fid = emit_drop_glue(
            &mut ctx,
            &mut module,
            &info,
            self_ty,
            Some(custom_fid),
            &[],
        )
        .expect("must emit glue");

        // Inspect instructions: entry block must contain a Call.
        let func = module.function(glue_fid);
        let entry_bid = llvm_ir::context::BlockId(0);
        let entry_bb = func.block(entry_bid);

        let has_call = entry_bb.body.iter().any(|&iid| {
            matches!(func.instr(iid).kind, InstrKind::Call { .. })
        });
        assert!(has_call, "glue function entry block must contain a call instruction");
    }

    #[test]
    fn emit_drop_glue_emits_gep_for_droppable_fields() {
        let mut ctx = Context::new();
        let mut module = Module::new("test");

        // Add a field drop-glue function first.
        let void_ty = ctx.void_ty;
        let ptr_ty = ctx.ptr_ty;
        let fn_ty = ctx.mk_fn_type(void_ty, vec![ptr_ty], false);
        let field_fn = llvm_ir::Function::new(
            "__drop_glue_FieldType",
            fn_ty,
            vec![llvm_ir::value::Argument { name: "self".to_string(), ty: ptr_ty, index: 0 }],
            Linkage::External,
        );
        let field_fid = module.add_function(field_fn);

        let field_ty = ctx.i64_ty;
        let self_ty = ctx.mk_struct_anon(vec![field_ty], false);

        let info = DropInfo {
            type_name: "Composite".to_string(),
            has_custom_drop: false,
            droppable_fields: vec![0],
        };

        let glue_fid = emit_drop_glue(
            &mut ctx,
            &mut module,
            &info,
            self_ty,
            None,
            &[(0, field_fid)],
        )
        .expect("must emit glue");

        // Entry block must contain a GEP.
        let func = module.function(glue_fid);
        let entry_bid = llvm_ir::context::BlockId(0);
        let entry_bb = func.block(entry_bid);

        let has_gep = entry_bb.body.iter().any(|&iid| {
            matches!(func.instr(iid).kind, InstrKind::GetElementPtr { .. })
        });
        assert!(has_gep, "glue for droppable fields must emit a GEP instruction");
    }

    #[test]
    fn emit_cleanup_block_branches_to_next() {
        let mut ctx = Context::new();
        let mut module = Module::new("test");

        // Set up a simple function.
        let void_ty = ctx.void_ty;
        let ptr_ty = ctx.ptr_ty;

        let mut builder = Builder::new(&mut ctx, &mut module);
        let fid = builder.add_function(
            "test_fn",
            void_ty,
            vec![ptr_ty],
            vec!["x".to_string()],
            false,
            Linkage::External,
        );
        let entry = builder.add_block("entry");
        let next = builder.add_block("next");

        // Add a trivial terminator to entry and next.
        builder.position_at_end(entry);
        let local_ptr = builder.get_arg(0);
        builder.build_br(next);

        builder.position_at_end(next);
        builder.build_ret_void();

        // Add a drop-glue function to reference.
        let drop_fn_ty = ctx.mk_fn_type(void_ty, vec![ptr_ty], false);
        let drop_fn = llvm_ir::Function::new(
            "__drop_glue_SomeType",
            drop_fn_ty,
            vec![llvm_ir::value::Argument { name: "self".to_string(), ty: ptr_ty, index: 0 }],
            Linkage::External,
        );
        let drop_fid = module.add_function(drop_fn);

        // Now emit the cleanup block.
        let func = module.function_mut(fid);
        let cleanup_bid = emit_cleanup_block(&mut ctx, func, local_ptr, drop_fid, next);

        // Verify the cleanup block ends with a Br to `next`.
        let func = module.function(fid);
        let cleanup_bb = func.block(cleanup_bid);
        let term_id = cleanup_bb.terminator.expect("cleanup block must have a terminator");
        let term = func.instr(term_id);

        assert!(
            matches!(term.kind, InstrKind::Br { dest } if dest == next),
            "cleanup block must terminate with an unconditional branch to next_block"
        );
    }
}
