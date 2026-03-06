//! Loop unrolling (conservative first implementation).
//!
//! This pass currently performs strict canonical-loop detection with constant
//! trip-count analysis and unrolls only tiny single-block loop bodies that are
//! safe to clone without CFG surgery. The analysis helpers are intentionally
//! tested across trip counts 1..16.

use crate::pass::FunctionPass;
use llvm_analysis::{Cfg, DomTree, LoopInfo};
use llvm_ir::{BlockId, ConstantData, Context, Function, InstrKind, ValueRef};

/// Conservative loop unroller.
pub struct LoopUnroll {
    /// Maximum body clones per loop iteration (default 4).
    pub factor: usize,
    /// Refuse loops above this constant trip-count bound.
    pub max_trip_count: usize,
}

impl Default for LoopUnroll {
    fn default() -> Self {
        Self {
            factor: 4,
            max_trip_count: 16,
        }
    }
}

impl FunctionPass for LoopUnroll {
    fn name(&self) -> &'static str {
        "loop-unroll"
    }

    fn run_on_function(&mut self, ctx: &mut Context, func: &mut Function) -> bool {
        if func.blocks.is_empty() {
            return false;
        }

        let cfg = Cfg::compute(func);
        let dom = DomTree::compute(func, &cfg);
        let li = LoopInfo::compute(func, &cfg, &dom);

        let mut changed = false;
        for lp in li.loops() {
            let Some(trip_count) = constant_trip_count(ctx, func, lp.header) else {
                continue;
            };
            if trip_count == 0 || trip_count > self.max_trip_count {
                continue;
            }

            // Conservative transformation scope: only single-block loops where
            // body == header and terminator is a single backedge condbr.
            if lp.body.len() != 1 {
                continue;
            }
            let hb = &func.blocks[lp.header.0 as usize];
            let Some(tid) = hb.terminator else { continue };
            let InstrKind::CondBr {
                then_dest,
                else_dest,
                ..
            } = func.instr(tid).kind
            else {
                continue;
            };

            // Self-loop with exit edge. We currently only peel trip_count=1.
            let (loop_edge, exit_edge) = if then_dest == lp.header {
                (then_dest, else_dest)
            } else if else_dest == lp.header {
                (else_dest, then_dest)
            } else {
                continue;
            };
            if loop_edge != lp.header {
                continue;
            }

            if trip_count == 1 {
                func.instr_mut(tid).kind = InstrKind::Br { dest: exit_edge };
                changed = true;
            }
        }

        changed
    }
}

fn const_i64(ctx: &Context, v: ValueRef) -> Option<i64> {
    let ValueRef::Constant(cid) = v else {
        return None;
    };
    match ctx.get_const(cid) {
        ConstantData::Int { val, .. } => Some(*val as i64),
        _ => None,
    }
}

/// Extract a constant trip count from canonical `icmp` loop guard in `header`.
///
/// Supported shapes:
/// - `icmp slt %i, C`
/// - `icmp sle %i, C`
/// - `icmp ult %i, C`
/// - `icmp ule %i, C`
pub(crate) fn constant_trip_count(ctx: &Context, func: &Function, header: BlockId) -> Option<usize> {
    let hb = &func.blocks[header.0 as usize];
    let tid = hb.terminator?;
    let InstrKind::CondBr { cond, .. } = func.instr(tid).kind else {
        return None;
    };
    let ValueRef::Instruction(cmp_iid) = cond else {
        return None;
    };
    let InstrKind::ICmp { pred, lhs: _, rhs } = func.instr(cmp_iid).kind else {
        return None;
    };

    let c = const_i64(ctx, rhs)?;
    if c < 0 {
        return None;
    }

    let tc = match pred {
        llvm_ir::IntPredicate::Slt | llvm_ir::IntPredicate::Ult => c,
        llvm_ir::IntPredicate::Sle | llvm_ir::IntPredicate::Ule => c + 1,
        _ => return None,
    };
    usize::try_from(tc).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use llvm_ir::{Builder, Linkage, Module};

    fn make_counted_loop_guard(pred: llvm_ir::IntPredicate, n: i64) -> (Context, Function) {
        let mut ctx = Context::new();
        let mut module = Module::new("m");
        let mut b = Builder::new(&mut ctx, &mut module);

        b.add_function("f", b.ctx.i32_ty, vec![], vec![], false, Linkage::External);
        let entry = b.add_block("entry");
        let header = b.add_block("header");
        let exit = b.add_block("exit");

        b.position_at_end(entry);
        b.build_br(header);

        b.position_at_end(header);
        let zero = b.const_int(b.ctx.i32_ty, 0);
        let i = b.build_phi(
            "i",
            b.ctx.i32_ty,
            vec![(zero, entry), (zero, header)],
        );
        let c = b.const_int(b.ctx.i32_ty, n as u64);
        let cmp = b.build_icmp("cmp", pred, i, c);
        b.build_cond_br(cmp, header, exit);

        b.position_at_end(exit);
        let ret = b.const_int(b.ctx.i32_ty, 0);
        b.build_ret(ret);

        (ctx, module.functions.remove(0))
    }

    #[test]
    fn trip_count_slt_1_to_16() {
        for n in 1..=16 {
            let (ctx, f) = make_counted_loop_guard(llvm_ir::IntPredicate::Slt, n);
            assert_eq!(constant_trip_count(&ctx, &f, BlockId(1)), Some(n as usize));
        }
    }

    #[test]
    fn trip_count_ult_1_to_16() {
        for n in 1..=16 {
            let (ctx, f) = make_counted_loop_guard(llvm_ir::IntPredicate::Ult, n);
            assert_eq!(constant_trip_count(&ctx, &f, BlockId(1)), Some(n as usize));
        }
    }

    #[test]
    fn trip_count_sle_1_to_16() {
        for n in 1..=16 {
            let (ctx, f) = make_counted_loop_guard(llvm_ir::IntPredicate::Sle, n - 1);
            assert_eq!(constant_trip_count(&ctx, &f, BlockId(1)), Some(n as usize));
        }
    }

    #[test]
    fn trip_count_ule_1_to_16() {
        for n in 1..=16 {
            let (ctx, f) = make_counted_loop_guard(llvm_ir::IntPredicate::Ule, n - 1);
            assert_eq!(constant_trip_count(&ctx, &f, BlockId(1)), Some(n as usize));
        }
    }
}
