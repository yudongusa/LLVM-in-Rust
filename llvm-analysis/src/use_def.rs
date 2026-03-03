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

use std::collections::HashMap;
use llvm_ir::{BlockId, InstrId, ValueRef, Function};

/// Use-def / def-use information for a single function.
pub struct UseDefInfo {
    /// Block in which each instruction is defined.
    instr_block: HashMap<InstrId, BlockId>,
    /// All use sites of each SSA value: (block, instruction) pairs.
    uses: HashMap<ValueRef, Vec<(BlockId, InstrId)>>,
}

impl UseDefInfo {
    /// Walk all instructions in `func` and collect definition and use info.
    pub fn compute(func: &Function) -> Self {
        let mut instr_block: HashMap<InstrId, BlockId> = HashMap::new();
        let mut uses: HashMap<ValueRef, Vec<(BlockId, InstrId)>> = HashMap::new();

        for (bi, bb) in func.blocks.iter().enumerate() {
            let bid = BlockId(bi as u32);
            for iid in bb.instrs() {
                instr_block.insert(iid, bid);
                for operand in func.instr(iid).kind.operands() {
                    uses.entry(operand).or_default().push((bid, iid));
                }
            }
        }

        UseDefInfo { instr_block, uses }
    }

    /// The block in which `id` is defined, or `None` if not found.
    pub fn def_block(&self, id: InstrId) -> Option<BlockId> {
        self.instr_block.get(&id).copied()
    }

    /// All use sites of `vref`: `(block, instruction)` pairs.
    /// Returns an empty slice if the value has no uses.
    pub fn uses_of(&self, vref: ValueRef) -> &[(BlockId, InstrId)] {
        self.uses.get(&vref).map(Vec::as_slice).unwrap_or(&[])
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
}
