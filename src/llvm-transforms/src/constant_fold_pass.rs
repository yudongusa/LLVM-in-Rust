//! Dedicated constant-folding function pass.
//!
//! This pass evaluates foldable instructions whose operands are compile-time
//! constants (via [`crate::constant_fold::try_fold`]) and rewrites downstream
//! uses to direct constants.

use crate::const_prop::{rpo, subst_kind};
use crate::constant_fold::try_fold;
use crate::pass::FunctionPass;
use llvm_ir::{Context, Function, InstrId, ValueRef};
use std::collections::HashMap;

/// Function pass that folds compile-time constant expressions.
pub struct ConstantFold;

impl FunctionPass for ConstantFold {
    fn name(&self) -> &'static str {
        "constant-fold"
    }

    fn run_on_function(&mut self, ctx: &mut Context, func: &mut Function) -> bool {
        if func.blocks.is_empty() {
            return false;
        }

        // Map InstrId -> folded constant replacement.
        let mut subst: HashMap<InstrId, ValueRef> = HashMap::new();

        for bi in rpo(func) {
            let body = func.blocks[bi].body.clone();
            for iid in body {
                if !subst.is_empty() {
                    let new_kind = subst_kind(func.instr(iid).kind.clone(), &subst);
                    func.instr_mut(iid).kind = new_kind;
                }
                let kind = func.instr(iid).kind.clone();
                if let Some(cid) = try_fold(ctx, &kind) {
                    subst.insert(iid, ValueRef::Constant(cid));
                }
            }
            if let Some(tid) = func.blocks[bi].terminator {
                if !subst.is_empty() {
                    let new_kind = subst_kind(func.instr(tid).kind.clone(), &subst);
                    func.instr_mut(tid).kind = new_kind;
                }
            }
        }

        if subst.is_empty() {
            return false;
        }

        for bb in &mut func.blocks {
            bb.body.retain(|id| !subst.contains_key(id));
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use llvm_ir::{Builder, Context, InstrKind, Linkage, Module, ValueRef};

    fn make_const_add_fn() -> (Context, Module) {
        let mut ctx = Context::new();
        let mut module = Module::new("const_add");
        let mut b = Builder::new(&mut ctx, &mut module);
        b.add_function("f", b.ctx.i32_ty, vec![], vec![], false, Linkage::External);
        let entry = b.add_block("entry");
        b.position_at_end(entry);
        let c2 = b.const_int(b.ctx.i32_ty, 2);
        let sum = b.build_add("sum", c2, c2);
        b.build_ret(sum);
        (ctx, module)
    }

    fn make_non_const_add_fn() -> (Context, Module) {
        let mut ctx = Context::new();
        let mut module = Module::new("non_const_add");
        let mut b = Builder::new(&mut ctx, &mut module);
        b.add_function(
            "f",
            b.ctx.i32_ty,
            vec![b.ctx.i32_ty],
            vec!["x".into()],
            false,
            Linkage::External,
        );
        let entry = b.add_block("entry");
        b.position_at_end(entry);
        let x = b.get_arg(0);
        let c2 = b.const_int(b.ctx.i32_ty, 2);
        let sum = b.build_add("sum", x, c2);
        b.build_ret(sum);
        (ctx, module)
    }

    #[test]
    fn folds_add_2_plus_2() {
        let (mut ctx, mut module) = make_const_add_fn();
        let mut pass = ConstantFold;
        let changed = pass.run_on_function(&mut ctx, &mut module.functions[0]);
        assert!(changed);
        assert_eq!(module.functions[0].blocks[0].body.len(), 0);
        let func = &module.functions[0];
        let tid = func.blocks[0].terminator.expect("terminator");
        match &func.instr(tid).kind {
            InstrKind::Ret {
                val: Some(ValueRef::Constant(cid)),
            } => match ctx.get_const(*cid) {
                llvm_ir::ConstantData::Int { val, .. } => assert_eq!(*val, 4),
                other => panic!("unexpected ret constant: {other:?}"),
            },
            other => panic!("expected ret constant, got {other:?}"),
        }
    }

    #[test]
    fn does_not_fold_non_constant_expression() {
        let (mut ctx, mut module) = make_non_const_add_fn();
        let mut pass = ConstantFold;
        let changed = pass.run_on_function(&mut ctx, &mut module.functions[0]);
        assert!(!changed);
        assert_eq!(module.functions[0].blocks[0].body.len(), 1);
    }
}
