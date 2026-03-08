//! Constant propagation pass.
//!
//! Walks every instruction in a function in **reverse post-order (RPO)** so
//! that each block's predecessors are generally visited before the block
//! itself.  When all operands of an instruction are constants it folds the
//! instruction to a constant (via `try_fold`), records the substitution, and
//! rewrites all downstream uses of that instruction result.  Folded
//! instructions are then dropped from block bodies.
//!
//! RPO traversal propagates constants through straight-line code and through
//! forward edges of loops in a single pass.  Back-edges (loop-carried
//! constants) require a second pass; use `PassManager::run_until_fixed_point`
//! when that is needed.

use crate::constant_fold::try_fold;
use crate::pass::FunctionPass;
use llvm_ir::{Context, Function, InstrId, InstrKind, ValueRef};
use std::collections::{HashMap, HashSet};

/// Constant propagation / constant folding pass.
pub struct ConstProp;

impl FunctionPass for ConstProp {
    fn name(&self) -> &'static str {
        "const-prop"
    }

    fn run_on_function(&mut self, ctx: &mut Context, func: &mut Function) -> bool {
        if func.blocks.is_empty() {
            return false;
        }

        // Map InstrId → its constant replacement (ValueRef::Constant).
        let mut subst: HashMap<InstrId, ValueRef> = HashMap::new();

        // Process blocks in RPO so each block's predecessors generally come first.
        for bi in rpo(func) {
            let body: Vec<InstrId> = func.blocks[bi].body.clone();
            for iid in body {
                // Apply pending substitutions to this instruction's operands.
                if !subst.is_empty() {
                    let new_kind = subst_kind(func.instr(iid).kind.clone(), &subst);
                    func.instr_mut(iid).kind = new_kind;
                }
                // Try to fold the (possibly updated) instruction.
                let kind = func.instr(iid).kind.clone();
                if let Some(cid) = try_fold(ctx, &kind) {
                    subst.insert(iid, ValueRef::Constant(cid));
                }
            }
            // Also propagate into the terminator.
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

        // Remove folded instructions from block bodies (they are now dead).
        for bb in &mut func.blocks {
            bb.body.retain(|id| !subst.contains_key(id));
        }
        true
    }
}

// ---------------------------------------------------------------------------
// Value substitution helper — replaces InstrId refs appearing in `subst`
// ---------------------------------------------------------------------------

/// Substitute ValueRef operands throughout `kind`, replacing every
/// `ValueRef::Instruction(id)` that appears in `subst` with `subst[id]`.
///
/// This function is `pub(crate)` so that `mem2reg` can reuse it.
pub(crate) fn subst_kind(kind: InstrKind, subst: &HashMap<InstrId, ValueRef>) -> InstrKind {
    let s = |v: ValueRef| -> ValueRef {
        if let ValueRef::Instruction(id) = v {
            subst.get(&id).copied().unwrap_or(v)
        } else {
            v
        }
    };

    match kind {
        // --- Integer arithmetic ---
        InstrKind::Add { flags, lhs, rhs } => InstrKind::Add {
            flags,
            lhs: s(lhs),
            rhs: s(rhs),
        },
        InstrKind::Sub { flags, lhs, rhs } => InstrKind::Sub {
            flags,
            lhs: s(lhs),
            rhs: s(rhs),
        },
        InstrKind::Mul { flags, lhs, rhs } => InstrKind::Mul {
            flags,
            lhs: s(lhs),
            rhs: s(rhs),
        },
        InstrKind::UDiv { exact, lhs, rhs } => InstrKind::UDiv {
            exact,
            lhs: s(lhs),
            rhs: s(rhs),
        },
        InstrKind::SDiv { exact, lhs, rhs } => InstrKind::SDiv {
            exact,
            lhs: s(lhs),
            rhs: s(rhs),
        },
        InstrKind::URem { lhs, rhs } => InstrKind::URem {
            lhs: s(lhs),
            rhs: s(rhs),
        },
        InstrKind::SRem { lhs, rhs } => InstrKind::SRem {
            lhs: s(lhs),
            rhs: s(rhs),
        },
        // --- Bitwise ---
        InstrKind::And { lhs, rhs } => InstrKind::And {
            lhs: s(lhs),
            rhs: s(rhs),
        },
        InstrKind::Or { lhs, rhs } => InstrKind::Or {
            lhs: s(lhs),
            rhs: s(rhs),
        },
        InstrKind::Xor { lhs, rhs } => InstrKind::Xor {
            lhs: s(lhs),
            rhs: s(rhs),
        },
        InstrKind::Shl { flags, lhs, rhs } => InstrKind::Shl {
            flags,
            lhs: s(lhs),
            rhs: s(rhs),
        },
        InstrKind::LShr { exact, lhs, rhs } => InstrKind::LShr {
            exact,
            lhs: s(lhs),
            rhs: s(rhs),
        },
        InstrKind::AShr { exact, lhs, rhs } => InstrKind::AShr {
            exact,
            lhs: s(lhs),
            rhs: s(rhs),
        },
        // --- Floating-point ---
        InstrKind::FAdd { flags, lhs, rhs } => InstrKind::FAdd {
            flags,
            lhs: s(lhs),
            rhs: s(rhs),
        },
        InstrKind::FSub { flags, lhs, rhs } => InstrKind::FSub {
            flags,
            lhs: s(lhs),
            rhs: s(rhs),
        },
        InstrKind::FMul { flags, lhs, rhs } => InstrKind::FMul {
            flags,
            lhs: s(lhs),
            rhs: s(rhs),
        },
        InstrKind::FDiv { flags, lhs, rhs } => InstrKind::FDiv {
            flags,
            lhs: s(lhs),
            rhs: s(rhs),
        },
        InstrKind::FRem { flags, lhs, rhs } => InstrKind::FRem {
            flags,
            lhs: s(lhs),
            rhs: s(rhs),
        },
        InstrKind::FNeg { flags, operand } => InstrKind::FNeg {
            flags,
            operand: s(operand),
        },
        // --- Comparisons ---
        InstrKind::ICmp { pred, lhs, rhs } => InstrKind::ICmp {
            pred,
            lhs: s(lhs),
            rhs: s(rhs),
        },
        InstrKind::FCmp {
            flags,
            pred,
            lhs,
            rhs,
        } => InstrKind::FCmp {
            flags,
            pred,
            lhs: s(lhs),
            rhs: s(rhs),
        },
        // --- Memory ---
        InstrKind::Alloca {
            alloc_ty,
            num_elements,
            align,
        } => InstrKind::Alloca {
            alloc_ty,
            num_elements: num_elements.map(s),
            align,
        },
        InstrKind::Load {
            ty,
            ptr,
            align,
            volatile,
        } => InstrKind::Load {
            ty,
            ptr: s(ptr),
            align,
            volatile,
        },
        InstrKind::Store {
            val,
            ptr,
            align,
            volatile,
        } => InstrKind::Store {
            val: s(val),
            ptr: s(ptr),
            align,
            volatile,
        },
        InstrKind::GetElementPtr {
            inbounds,
            base_ty,
            ptr,
            indices,
        } => InstrKind::GetElementPtr {
            inbounds,
            base_ty,
            ptr: s(ptr),
            indices: indices.into_iter().map(s).collect(),
        },
        // --- Casts ---
        InstrKind::Trunc { val, to } => InstrKind::Trunc { val: s(val), to },
        InstrKind::ZExt { val, to } => InstrKind::ZExt { val: s(val), to },
        InstrKind::SExt { val, to } => InstrKind::SExt { val: s(val), to },
        InstrKind::FPTrunc { val, to } => InstrKind::FPTrunc { val: s(val), to },
        InstrKind::FPExt { val, to } => InstrKind::FPExt { val: s(val), to },
        InstrKind::FPToUI { val, to } => InstrKind::FPToUI { val: s(val), to },
        InstrKind::FPToSI { val, to } => InstrKind::FPToSI { val: s(val), to },
        InstrKind::UIToFP { val, to } => InstrKind::UIToFP { val: s(val), to },
        InstrKind::SIToFP { val, to } => InstrKind::SIToFP { val: s(val), to },
        InstrKind::PtrToInt { val, to } => InstrKind::PtrToInt { val: s(val), to },
        InstrKind::IntToPtr { val, to } => InstrKind::IntToPtr { val: s(val), to },
        InstrKind::BitCast { val, to } => InstrKind::BitCast { val: s(val), to },
        InstrKind::AddrSpaceCast { val, to } => InstrKind::AddrSpaceCast { val: s(val), to },
        // --- Misc ---
        InstrKind::Select {
            cond,
            then_val,
            else_val,
        } => InstrKind::Select {
            cond: s(cond),
            then_val: s(then_val),
            else_val: s(else_val),
        },
        InstrKind::Phi { ty, incoming } => InstrKind::Phi {
            ty,
            incoming: incoming.into_iter().map(|(v, b)| (s(v), b)).collect(),
        },
        InstrKind::ExtractValue { aggregate, indices } => InstrKind::ExtractValue {
            aggregate: s(aggregate),
            indices,
        },
        InstrKind::InsertValue {
            aggregate,
            val,
            indices,
        } => InstrKind::InsertValue {
            aggregate: s(aggregate),
            val: s(val),
            indices,
        },
        InstrKind::ExtractElement { vec, idx } => InstrKind::ExtractElement {
            vec: s(vec),
            idx: s(idx),
        },
        InstrKind::InsertElement { vec, val, idx } => InstrKind::InsertElement {
            vec: s(vec),
            val: s(val),
            idx: s(idx),
        },
        InstrKind::ShuffleVector { v1, v2, mask } => InstrKind::ShuffleVector {
            v1: s(v1),
            v2: s(v2),
            mask,
        },
        // --- Call ---
        InstrKind::Call {
            tail,
            callee_ty,
            callee,
            args,
        } => InstrKind::Call {
            tail,
            callee_ty,
            callee: s(callee),
            args: args.into_iter().map(s).collect(),
        },
        // --- Terminators ---
        InstrKind::Ret { val } => InstrKind::Ret { val: val.map(s) },
        InstrKind::Br { dest } => InstrKind::Br { dest },
        InstrKind::CondBr {
            cond,
            then_dest,
            else_dest,
        } => InstrKind::CondBr {
            cond: s(cond),
            then_dest,
            else_dest,
        },
        InstrKind::Switch {
            val,
            default,
            cases,
        } => InstrKind::Switch {
            val: s(val),
            default,
            cases: cases.into_iter().map(|(v, b)| (s(v), b)).collect(),
        },
        InstrKind::Unreachable => InstrKind::Unreachable,
    }
}

// ---------------------------------------------------------------------------
// RPO helper — produces block indices in reverse post-order
// ---------------------------------------------------------------------------

/// Returns block indices of `func` in reverse post-order (RPO) from the
/// entry block (index 0).
///
/// RPO ensures that for any non-back-edge A→B in the CFG, A appears before B
/// in the returned sequence.  This means constant values defined in a block
/// are available for substitution in successor blocks in the same pass,
/// maximising the number of folds performed per iteration.
///
/// Unreachable blocks are appended at the end (in their stored order) so that
/// the pass still processes them for correctness.
pub(crate) fn rpo(func: &Function) -> Vec<usize> {
    let n = func.blocks.len();
    let mut visited: HashSet<usize> = HashSet::with_capacity(n);
    let mut post_order: Vec<usize> = Vec::with_capacity(n);

    // Iterative DFS to avoid stack overflow on deep CFGs.
    let mut stack: Vec<(usize, bool)> = vec![(0, false)]; // (block_idx, post-visit?)
    while let Some((bi, post)) = stack.pop() {
        if post {
            post_order.push(bi);
            continue;
        }
        if visited.contains(&bi) {
            continue;
        }
        visited.insert(bi);
        // Push post-visit marker first, then successors in reverse so we
        // process them left-to-right.
        stack.push((bi, true));
        if let Some(tid) = func.blocks[bi].terminator {
            let succs = func.instr(tid).successors();
            for &succ in succs.iter().rev() {
                let si = succ.0 as usize;
                if si < n && !visited.contains(&si) {
                    stack.push((si, false));
                }
            }
        }
    }

    // Reverse post-order.
    post_order.reverse();

    // Append any unreachable blocks not visited by the DFS.
    for bi in 0..n {
        if !visited.contains(&bi) {
            post_order.push(bi);
        }
    }

    post_order
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pass::FunctionPass;
    use llvm_ir::{Builder, Context, Linkage, Module, ValueRef};

    // Build: f() -> i32 { ret (2 + 3) }  — the add folds to 5
    fn make_const_add_fn() -> (Context, Module) {
        let mut ctx = Context::new();
        let mut module = Module::new("test");
        let mut b = Builder::new(&mut ctx, &mut module);
        b.add_function("f", b.ctx.i32_ty, vec![], vec![], false, Linkage::External);
        let entry = b.add_block("entry");
        b.position_at_end(entry);
        let c2 = b.const_int(b.ctx.i32_ty, 2);
        let c3 = b.const_int(b.ctx.i32_ty, 3);
        let sum = b.build_add("sum", c2, c3);
        b.build_ret(sum);
        (ctx, module)
    }

    #[test]
    fn const_prop_folds_add() {
        let (mut ctx, mut module) = make_const_add_fn();
        // Before: body has 'sum = add 2, 3'.
        assert_eq!(module.functions[0].blocks[0].body.len(), 1);

        let mut pass = ConstProp;
        let changed = pass.run_on_function(&mut ctx, &mut module.functions[0]);
        assert!(changed);

        // After: 'sum' folded away; body is empty.
        assert_eq!(
            module.functions[0].blocks[0].body.len(),
            0,
            "folded add must be removed from body"
        );

        // The ret terminator should now reference the constant 5 directly.
        let func = &module.functions[0];
        let tid = func.blocks[0].terminator.unwrap();
        if let llvm_ir::InstrKind::Ret { val: Some(v) } = &func.instr(tid).kind {
            if let ValueRef::Constant(cid) = v {
                assert_eq!(
                    ctx.get_const(*cid),
                    &llvm_ir::ConstantData::Int {
                        ty: ctx.i32_ty,
                        val: 5
                    }
                );
            } else {
                panic!("ret operand should be a constant");
            }
        } else {
            panic!("terminator should be ret with value");
        }
    }

    #[test]
    fn const_prop_chain() {
        // f() -> i32 { a = 2+3; b = a*10; ret b }  → 50
        let mut ctx = Context::new();
        let mut module = Module::new("test");
        let mut b = Builder::new(&mut ctx, &mut module);
        b.add_function("f", b.ctx.i32_ty, vec![], vec![], false, Linkage::External);
        let entry = b.add_block("entry");
        b.position_at_end(entry);
        let c2 = b.const_int(b.ctx.i32_ty, 2);
        let c3 = b.const_int(b.ctx.i32_ty, 3);
        let c10 = b.const_int(b.ctx.i32_ty, 10);
        let sum = b.build_add("sum", c2, c3);
        let prod = b.build_mul("prod", sum, c10);
        b.build_ret(prod);

        let mut pass = ConstProp;
        pass.run_on_function(&mut ctx, &mut module.functions[0]);

        let func = &module.functions[0];
        assert_eq!(
            func.blocks[0].body.len(),
            0,
            "both sum and prod should be folded"
        );
        let tid = func.blocks[0].terminator.unwrap();
        if let llvm_ir::InstrKind::Ret {
            val: Some(ValueRef::Constant(cid)),
        } = &func.instr(tid).kind
        {
            assert_eq!(
                ctx.get_const(*cid),
                &llvm_ir::ConstantData::Int {
                    ty: ctx.i32_ty,
                    val: 50
                }
            );
        } else {
            panic!("expected ret with constant 50");
        }
    }

    #[test]
    fn const_prop_across_blocks_rpo() {
        // Two blocks: entry defines a constant, then branches to exit which uses it.
        //   entry: a = add 2, 3; br exit
        //   exit:  b = add a, 10; ret b
        // With RPO ordering, entry is processed before exit so `a` is folded (=5)
        // and then `b = add 5, 10` is also folded (=15).
        let mut ctx = Context::new();
        let mut module = Module::new("test");
        let mut b = Builder::new(&mut ctx, &mut module);
        b.add_function("f", b.ctx.i32_ty, vec![], vec![], false, Linkage::External);

        let entry = b.add_block("entry");
        let exit = b.add_block("exit");

        b.position_at_end(entry);
        let c2 = b.const_int(b.ctx.i32_ty, 2);
        let c3 = b.const_int(b.ctx.i32_ty, 3);
        let a = b.build_add("a", c2, c3);
        b.build_br(exit);

        b.position_at_end(exit);
        let c10 = b.const_int(b.ctx.i32_ty, 10);
        let bv = b.build_add("b", a, c10);
        b.build_ret(bv);

        let mut pass = ConstProp;
        pass.run_on_function(&mut ctx, &mut module.functions[0]);

        let func = &module.functions[0];
        // Both `a` and `b` should be folded away.
        assert_eq!(
            func.blocks[0].body.len(),
            0,
            "`a` should be folded in entry"
        );
        assert_eq!(func.blocks[1].body.len(), 0, "`b` should be folded in exit");
        // ret in exit block should reference constant 15.
        let tid = func.blocks[1].terminator.unwrap();
        if let llvm_ir::InstrKind::Ret {
            val: Some(ValueRef::Constant(cid)),
        } = &func.instr(tid).kind
        {
            assert_eq!(
                ctx.get_const(*cid),
                &llvm_ir::ConstantData::Int {
                    ty: ctx.i32_ty,
                    val: 15
                }
            );
        } else {
            panic!("expected ret with constant 15");
        }
    }

    #[test]
    fn rpo_order_entry_before_successor() {
        // Verify that rpo() returns entry block (0) before its successor.
        let mut ctx = Context::new();
        let mut module = Module::new("test");
        let mut b = Builder::new(&mut ctx, &mut module);
        b.add_function("f", b.ctx.i32_ty, vec![], vec![], false, Linkage::External);
        let entry = b.add_block("entry");
        let exit = b.add_block("exit");
        b.position_at_end(entry);
        b.build_br(exit);
        b.position_at_end(exit);
        let c = b.const_int(b.ctx.i32_ty, 0);
        b.build_ret(c);

        let func = &module.functions[0];
        let order = rpo(func);
        let entry_pos = order.iter().position(|&i| i == entry.0 as usize).unwrap();
        let exit_pos = order.iter().position(|&i| i == exit.0 as usize).unwrap();
        assert!(entry_pos < exit_pos, "entry must come before exit in RPO");
    }
}
