//! Use-def and def-use chains.
//!
//! `UseDefInfo::compute` walks every instruction in a function and builds:
//! - A map from each `InstrId` to the `BlockId` that contains its definition.
//! - A map from each `ValueRef` to the list of `(BlockId, InstrId)` pairs
//!   that use it as an operand.
//!
//! Only instruction-produced values are tracked as definitions. Arguments are
//! always available at function entry; constants and globals have no
//! definition block.
//!
//! # Phi operand semantics
//!
//! In SSA form, a phi incoming value `[v, %pred]` is semantically *used at
//! the end of the predecessor block `%pred`*, not at the phi's own block.
//! `UseDefInfo` exposes both views:
//!
//! | Accessor | Block recorded | Correct for |
//! |----------|---------------|-------------|
//! | `uses_of` | phi's own block | DCE (`is_dead`), simple def-use traversal |
//! | `phi_uses_of` | predecessor block | liveness analysis, mem2reg, SSA destruction |

use std::collections::HashMap;
use llvm_ir::{BlockId, InstrId, InstrKind, ValueRef, Function};

/// Use-def / def-use information for a single function.
pub struct UseDefInfo {
    /// Block in which each instruction is defined.
    instr_block: HashMap<InstrId, BlockId>,
    /// All use sites of each SSA value: `(block, instruction)` pairs.
    ///
    /// For phi incoming values the block is the **phi's own block**, not the
    /// predecessor. This is correct for dead-code elimination but not for
    /// liveness analysis. Use [`phi_uses_of`](Self::phi_uses_of) when
    /// predecessor-block semantics are required.
    uses: HashMap<ValueRef, Vec<(BlockId, InstrId)>>,
    /// Phi-specific use sites with **predecessor-block** semantics.
    ///
    /// For a phi `%v = phi [%a, %pred0], [%b, %pred1]`, this map records:
    /// - `%a` → `[(pred0, phi_iid)]`
    /// - `%b` → `[(pred1, phi_iid)]`
    ///
    /// Only phi incoming values appear here. Non-phi operands are absent.
    phi_uses: HashMap<ValueRef, Vec<(BlockId, InstrId)>>,
}

impl UseDefInfo {
    /// Walk all instructions in `func` and collect definition and use info.
    pub fn compute(func: &Function) -> Self {
        let mut instr_block: HashMap<InstrId, BlockId> = HashMap::new();
        let mut uses: HashMap<ValueRef, Vec<(BlockId, InstrId)>> = HashMap::new();
        let mut phi_uses: HashMap<ValueRef, Vec<(BlockId, InstrId)>> = HashMap::new();

        for (bi, bb) in func.blocks.iter().enumerate() {
            let bid = BlockId(bi as u32);
            for iid in bb.instrs() {
                instr_block.insert(iid, bid);
                let instr = func.instr(iid);
                match &instr.kind {
                    InstrKind::Phi { incoming, .. } => {
                        for (val, pred) in incoming {
                            // Record at phi's block for DCE / is_dead correctness.
                            uses.entry(*val).or_default().push((bid, iid));
                            // Record at predecessor block for liveness / mem2reg.
                            phi_uses.entry(*val).or_default().push((*pred, iid));
                        }
                    }
                    _ => {
                        for operand in instr.kind.operands() {
                            uses.entry(operand).or_default().push((bid, iid));
                        }
                    }
                }
            }
        }

        UseDefInfo { instr_block, uses, phi_uses }
    }

    /// The block in which `id` is defined, or `None` if not found.
    pub fn def_block(&self, id: InstrId) -> Option<BlockId> {
        self.instr_block.get(&id).copied()
    }

    /// All use sites of `vref`: `(block, instruction)` pairs.
    ///
    /// For phi incoming values the block is the **phi's own block**, not the
    /// predecessor. Use [`phi_uses_of`](Self::phi_uses_of) when correct
    /// predecessor-block semantics are needed (liveness, mem2reg).
    ///
    /// Returns an empty slice if the value has no uses.
    pub fn uses_of(&self, vref: ValueRef) -> &[(BlockId, InstrId)] {
        self.uses.get(&vref).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Phi-specific use sites of `vref` with **predecessor-block** semantics.
    ///
    /// For each `phi [vref, %pred]` instruction that uses `vref`, returns a
    /// `(pred, phi_instr_id)` pair. This is the correct view for SSA liveness
    /// analysis (the value must be live-out of `pred`) and for phi elimination
    /// (copies are inserted at the end of `pred`).
    ///
    /// Returns an empty slice if `vref` does not appear in any phi incoming list.
    pub fn phi_uses_of(&self, vref: ValueRef) -> &[(BlockId, InstrId)] {
        self.phi_uses.get(&vref).map(Vec::as_slice).unwrap_or(&[])
    }

    /// `true` if `vref` has no recorded uses (dead value).
    pub fn is_dead(&self, vref: ValueRef) -> bool {
        self.uses_of(vref).is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use llvm_ir::{ArgId, Context, Module, Builder, Linkage};

    fn make_add_fn() -> (Context, Module) {
        let mut ctx = Context::new();
        let mut module = Module::new("test");
        let mut b = Builder::new(&mut ctx, &mut module);
        b.add_function("add", b.ctx.i32_ty,
            vec![b.ctx.i32_ty, b.ctx.i32_ty],
            vec!["a".into(), "b".into()], false, Linkage::External);
        let entry = b.add_block("entry");
        b.position_at_end(entry);
        let a = b.get_arg(0);
        let bv = b.get_arg(1);
        let sum = b.build_add("sum", a, bv);
        b.build_ret(sum);
        (ctx, module)
    }

    #[test]
    fn use_def_basic() {
        let (_ctx, module) = make_add_fn();
        let func = &module.functions[0];
        let info = UseDefInfo::compute(func);

        // sum (InstrId 0) is defined in block 0.
        assert_eq!(info.def_block(InstrId(0)), Some(BlockId(0)));

        // ret uses sum.
        let sum_ref = ValueRef::Instruction(InstrId(0));
        assert_eq!(info.uses_of(sum_ref).len(), 1);

        // sum uses both args.
        assert_eq!(info.uses_of(ValueRef::Argument(ArgId(0))).len(), 1);
        assert_eq!(info.uses_of(ValueRef::Argument(ArgId(1))).len(), 1);
    }

    #[test]
    fn use_def_dead_value() {
        let mut ctx = Context::new();
        let mut module = Module::new("test");
        let mut b = Builder::new(&mut ctx, &mut module);
        b.add_function("f", b.ctx.i32_ty, vec![b.ctx.i32_ty],
            vec!["x".into()], false, Linkage::External);
        let entry = b.add_block("entry");
        b.position_at_end(entry);
        let x = b.get_arg(0);
        let _dead = b.build_add("dead", x, x); // never used
        b.build_ret(x);

        let func = &module.functions[0];
        let info = UseDefInfo::compute(func);

        // dead (InstrId 0) has no uses.
        assert!(info.is_dead(ValueRef::Instruction(InstrId(0))));
        // x is used by dead (×2) and ret (×1).
        assert_eq!(info.uses_of(ValueRef::Argument(ArgId(0))).len(), 3);
    }

    #[test]
    fn use_def_multi_block() {
        let mut ctx = Context::new();
        let mut module = Module::new("test");
        let mut b = Builder::new(&mut ctx, &mut module);
        b.add_function("f", b.ctx.void_ty, vec![b.ctx.i1_ty],
            vec!["c".into()], false, Linkage::External);
        let entry = b.add_block("entry");
        let then_bb = b.add_block("then");
        let else_bb = b.add_block("else");
        b.position_at_end(entry);
        let cond = b.get_arg(0);
        b.build_cond_br(cond, then_bb, else_bb);
        b.position_at_end(then_bb);
        b.build_ret_void();
        b.position_at_end(else_bb);
        b.build_ret_void();

        let func = &module.functions[0];
        let info = UseDefInfo::compute(func);

        // cond (ArgId 0) is used by the br in block 0.
        let uses = info.uses_of(ValueRef::Argument(ArgId(0)));
        assert_eq!(uses.len(), 1);
        assert_eq!(uses[0].0, BlockId(0));
    }

    // Build a function with a phi node:
    //
    //   entry (b0):  br i1 %cond, then, else
    //   then  (b1):  br merge
    //   else  (b2):  br merge
    //   merge (b3):  %v = phi i32 [%a, %then], [%bv, %else]
    //                ret %v
    //
    // %a (ArgId 1) flows from b1; %bv (ArgId 2) flows from b2.
    fn make_phi_fn() -> (Context, Module) {
        let mut ctx = Context::new();
        let mut module = Module::new("test");
        let mut b = Builder::new(&mut ctx, &mut module);
        b.add_function("phi_fn", b.ctx.i32_ty,
            vec![b.ctx.i1_ty, b.ctx.i32_ty, b.ctx.i32_ty],
            vec!["cond".into(), "a".into(), "bv".into()], false, Linkage::External);
        let entry = b.add_block("entry");
        let then_bb = b.add_block("then");
        let else_bb = b.add_block("else");
        let merge = b.add_block("merge");

        b.position_at_end(entry);
        let cond = b.get_arg(0);
        b.build_cond_br(cond, then_bb, else_bb);

        b.position_at_end(then_bb);
        b.build_br(merge);

        b.position_at_end(else_bb);
        b.build_br(merge);

        b.position_at_end(merge);
        let a = b.get_arg(1);
        let bv = b.get_arg(2);
        let v = b.build_phi("v", b.ctx.i32_ty, vec![(a, then_bb), (bv, else_bb)]);
        b.build_ret(v);

        (ctx, module)
    }

    #[test]
    fn phi_uses_of_records_predecessor_block() {
        let (_ctx, module) = make_phi_fn();
        let func = &module.functions[0];
        let info = UseDefInfo::compute(func);

        // %a (ArgId 1) flows into phi from then_bb (BlockId 1).
        // %bv (ArgId 2) flows into phi from else_bb (BlockId 2).
        let a_ref = ValueRef::Argument(ArgId(1));
        let b_ref = ValueRef::Argument(ArgId(2));

        let a_phi_uses = info.phi_uses_of(a_ref);
        assert_eq!(a_phi_uses.len(), 1,
            "a should appear in exactly one phi incoming list");
        assert_eq!(a_phi_uses[0].0, BlockId(1),
            "a's phi use should be at predecessor then_bb (block 1), not merge");

        let b_phi_uses = info.phi_uses_of(b_ref);
        assert_eq!(b_phi_uses.len(), 1);
        assert_eq!(b_phi_uses[0].0, BlockId(2),
            "bv's phi use should be at predecessor else_bb (block 2), not merge");
    }

    #[test]
    fn uses_of_phi_operand_still_at_phi_block() {
        // uses_of() must still report phi operands at the phi's own block so
        // that is_dead() and DCE-style consumers continue to work correctly.
        let (_ctx, module) = make_phi_fn();
        let func = &module.functions[0];
        let info = UseDefInfo::compute(func);

        let a_ref = ValueRef::Argument(ArgId(1));
        let uses = info.uses_of(a_ref);
        // %a is used by the phi in merge (BlockId 3).
        assert_eq!(uses.len(), 1);
        assert_eq!(uses[0].0, BlockId(3),
            "uses_of should record phi operands at the phi's own block (merge = block 3)");

        // %a is not dead — it appears in a phi.
        assert!(!info.is_dead(a_ref));
    }

    #[test]
    fn non_phi_operands_absent_from_phi_uses() {
        // Non-phi instructions must not pollute the phi_uses map.
        let (_ctx, module) = make_add_fn();
        let func = &module.functions[0];
        let info = UseDefInfo::compute(func);

        // Neither arg appears in any phi — phi_uses_of must return empty.
        assert!(info.phi_uses_of(ValueRef::Argument(ArgId(0))).is_empty());
        assert!(info.phi_uses_of(ValueRef::Argument(ArgId(1))).is_empty());
    }
}
