//! Dead-code elimination (DCE).
//!
//! Removes instructions whose results are never used and which have no
//! observable side effects.  A single scan is enough; iterate via the
//! `PassManager` for a full fixed-point.
//!
//! Side-effecting instructions (`Store`, `Call`, `Load`, `Alloca`,
//! terminators) are never removed even if their results are unused.

use std::collections::HashSet;
use llvm_ir::{Context, Function, InstrId, InstrKind, ValueRef};
use llvm_analysis::UseDefInfo;
use crate::pass::FunctionPass;

/// Dead-code elimination pass.
pub struct DeadCodeElim;

impl FunctionPass for DeadCodeElim {
    fn name(&self) -> &'static str { "dce" }

    fn run_on_function(&mut self, _ctx: &mut Context, func: &mut Function) -> bool {
        let info = UseDefInfo::compute(func);

        let dead: HashSet<InstrId> = func
            .instructions
            .iter()
            .enumerate()
            .filter_map(|(i, instr)| {
                let iid = InstrId(i as u32);
                if is_dce_safe(&instr.kind) && info.is_dead(ValueRef::Instruction(iid)) {
                    Some(iid)
                } else {
                    None
                }
            })
            .collect();

        if dead.is_empty() {
            return false;
        }

        for bb in &mut func.blocks {
            bb.body.retain(|id| !dead.contains(id));
        }
        true
    }
}

/// Returns `true` if an unused instruction with this kind can safely be deleted.
///
/// Pure instructions (arithmetic, comparisons, casts, etc.) are safe.
/// Instructions with observable side effects (`Store`, `Call`, `Load`,
/// `Alloca`, terminators) are kept even when the result is dead.
pub fn is_dce_safe(kind: &InstrKind) -> bool {
    !matches!(
        kind,
        InstrKind::Alloca { .. }
            | InstrKind::Load { .. }
            | InstrKind::Store { .. }
            | InstrKind::Call { .. }
            | InstrKind::Ret { .. }
            | InstrKind::Br { .. }
            | InstrKind::CondBr { .. }
            | InstrKind::Switch { .. }
            | InstrKind::Unreachable
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use llvm_ir::{
        ArgId, Builder, Context, InstrKind, Linkage, Module, ValueRef,
    };
    use crate::pass::FunctionPass;

    // Build:  f(i32 %x) -> i32 { dead = add %x, %x; ret %x }
    fn make_dead_fn() -> (Context, Module) {
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
        (ctx, module)
    }

    #[test]
    fn dce_removes_dead_add() {
        let (mut ctx, mut module) = make_dead_fn();
        let func = &module.functions[0];
        // Before DCE: 2 instructions in body (add + ret).
        let body_before = func.blocks[0].body.len();
        assert_eq!(body_before, 1, "one non-term instr (add)");

        let mut pass = DeadCodeElim;
        let changed = pass.run_on_function(&mut ctx, &mut module.functions[0]);
        assert!(changed);
        assert_eq!(module.functions[0].blocks[0].body.len(), 0,
            "add removed; body should be empty");
    }

    #[test]
    fn dce_keeps_used_instr() {
        let mut ctx = Context::new();
        let mut module = Module::new("test");
        let mut b = Builder::new(&mut ctx, &mut module);
        b.add_function("g", b.ctx.i32_ty, vec![b.ctx.i32_ty, b.ctx.i32_ty],
            vec!["a".into(), "b".into()], false, Linkage::External);
        let entry = b.add_block("entry");
        b.position_at_end(entry);
        let a = b.get_arg(0);
        let bv = b.get_arg(1);
        let sum = b.build_add("sum", a, bv);
        b.build_ret(sum); // sum is used

        let mut pass = DeadCodeElim;
        let changed = pass.run_on_function(&mut ctx, &mut module.functions[0]);
        assert!(!changed);
        assert_eq!(module.functions[0].blocks[0].body.len(), 1, "sum must remain");
    }

    #[test]
    fn dce_safe_classification() {
        // Terminators / memory ops must not be considered safe.
        assert!(!is_dce_safe(&InstrKind::Unreachable));
        assert!(!is_dce_safe(&InstrKind::Ret { val: None }));
        assert!(!is_dce_safe(&InstrKind::Store {
            val: ValueRef::Argument(ArgId(0)),
            ptr: ValueRef::Argument(ArgId(1)),
            align: None,
            volatile: false,
        }));
        // Pure arithmetic is safe.
        assert!(is_dce_safe(&InstrKind::Add {
            flags: llvm_ir::IntArithFlags::default(),
            lhs: ValueRef::Argument(ArgId(0)),
            rhs: ValueRef::Argument(ArgId(0)),
        }));
    }
}
