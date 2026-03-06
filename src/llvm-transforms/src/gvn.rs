//! Global Value Numbering (GVN).
//!
//! This pass performs:
//! - local value numbering within a block
//! - dominator-tree propagation across blocks
//! - conservative redundant load elimination when memory is unchanged

use crate::const_prop::subst_kind;
use crate::pass::FunctionPass;
use llvm_analysis::{Cfg, DomTree};
use llvm_ir::{
    BlockId, Context, FastMathFlags, FloatPredicate, Function, InstrId, InstrKind, IntArithFlags,
    IntPredicate, TypeId, ValueRef,
};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum ExprKey {
    Add(u8, ValueRef, ValueRef),
    Sub(u8, ValueRef, ValueRef),
    Mul(u8, ValueRef, ValueRef),
    UDiv(bool, ValueRef, ValueRef),
    SDiv(bool, ValueRef, ValueRef),
    URem(ValueRef, ValueRef),
    SRem(ValueRef, ValueRef),
    And(ValueRef, ValueRef),
    Or(ValueRef, ValueRef),
    Xor(ValueRef, ValueRef),
    Shl(u8, ValueRef, ValueRef),
    LShr(bool, ValueRef, ValueRef),
    AShr(bool, ValueRef, ValueRef),
    ICmp(u8, ValueRef, ValueRef),
    Select(ValueRef, ValueRef, ValueRef),
    Trunc(ValueRef, TypeId),
    ZExt(ValueRef, TypeId),
    SExt(ValueRef, TypeId),
    BitCast(ValueRef, TypeId),
    PtrToInt(ValueRef, TypeId),
    IntToPtr(ValueRef, TypeId),
    FPTrunc(ValueRef, TypeId),
    FPExt(ValueRef, TypeId),
    FPToUI(ValueRef, TypeId),
    FPToSI(ValueRef, TypeId),
    UIToFP(ValueRef, TypeId),
    SIToFP(ValueRef, TypeId),
    AddrSpaceCast(ValueRef, TypeId),
    FAdd(u16, ValueRef, ValueRef),
    FSub(u16, ValueRef, ValueRef),
    FMul(u16, ValueRef, ValueRef),
    FDiv(u16, ValueRef, ValueRef),
    FRem(u16, ValueRef, ValueRef),
    FNeg(u16, ValueRef),
    FCmp(u16, u8, ValueRef, ValueRef),
}

fn int_flags_bits(flags: IntArithFlags) -> u8 {
    (if flags.nuw { 1 } else { 0 }) | ((if flags.nsw { 1 } else { 0 }) << 1)
}

fn fast_math_bits(flags: FastMathFlags) -> u16 {
    (if flags.nnan { 1 } else { 0 })
        | ((if flags.ninf { 1 } else { 0 }) << 1)
        | ((if flags.nsz { 1 } else { 0 }) << 2)
        | ((if flags.arcp { 1 } else { 0 }) << 3)
        | ((if flags.contract { 1 } else { 0 }) << 4)
        | ((if flags.afn { 1 } else { 0 }) << 5)
        | ((if flags.reassoc { 1 } else { 0 }) << 6)
        | ((if flags.fast { 1 } else { 0 }) << 7)
}

fn int_pred_bits(pred: IntPredicate) -> u8 {
    match pred {
        IntPredicate::Eq => 0,
        IntPredicate::Ne => 1,
        IntPredicate::Ugt => 2,
        IntPredicate::Uge => 3,
        IntPredicate::Ult => 4,
        IntPredicate::Ule => 5,
        IntPredicate::Sgt => 6,
        IntPredicate::Sge => 7,
        IntPredicate::Slt => 8,
        IntPredicate::Sle => 9,
    }
}

fn float_pred_bits(pred: FloatPredicate) -> u8 {
    match pred {
        FloatPredicate::False => 0,
        FloatPredicate::Oeq => 1,
        FloatPredicate::Ogt => 2,
        FloatPredicate::Oge => 3,
        FloatPredicate::Olt => 4,
        FloatPredicate::Ole => 5,
        FloatPredicate::One => 6,
        FloatPredicate::Ord => 7,
        FloatPredicate::Uno => 8,
        FloatPredicate::Ueq => 9,
        FloatPredicate::Ugt => 10,
        FloatPredicate::Uge => 11,
        FloatPredicate::Ult => 12,
        FloatPredicate::Ule => 13,
        FloatPredicate::Une => 14,
        FloatPredicate::True => 15,
    }
}

fn value_rank(v: ValueRef) -> (u8, u32) {
    match v {
        ValueRef::Instruction(id) => (0, id.0),
        ValueRef::Argument(id) => (1, id.0),
        ValueRef::Constant(id) => (2, id.0),
        ValueRef::Global(id) => (3, id.0),
    }
}

fn order_pair(a: ValueRef, b: ValueRef) -> (ValueRef, ValueRef) {
    if value_rank(a) <= value_rank(b) {
        (a, b)
    } else {
        (b, a)
    }
}

fn expr_key(kind: &InstrKind) -> Option<ExprKey> {
    use InstrKind::*;
    Some(match kind {
        Add { flags, lhs, rhs } => {
            let (l, r) = order_pair(*lhs, *rhs);
            ExprKey::Add(int_flags_bits(*flags), l, r)
        }
        Sub { flags, lhs, rhs } => ExprKey::Sub(int_flags_bits(*flags), *lhs, *rhs),
        Mul { flags, lhs, rhs } => {
            let (l, r) = order_pair(*lhs, *rhs);
            ExprKey::Mul(int_flags_bits(*flags), l, r)
        }
        UDiv { exact, lhs, rhs } => ExprKey::UDiv(*exact, *lhs, *rhs),
        SDiv { exact, lhs, rhs } => ExprKey::SDiv(*exact, *lhs, *rhs),
        URem { lhs, rhs } => ExprKey::URem(*lhs, *rhs),
        SRem { lhs, rhs } => ExprKey::SRem(*lhs, *rhs),
        And { lhs, rhs } => {
            let (l, r) = order_pair(*lhs, *rhs);
            ExprKey::And(l, r)
        }
        Or { lhs, rhs } => {
            let (l, r) = order_pair(*lhs, *rhs);
            ExprKey::Or(l, r)
        }
        Xor { lhs, rhs } => {
            let (l, r) = order_pair(*lhs, *rhs);
            ExprKey::Xor(l, r)
        }
        Shl { flags, lhs, rhs } => ExprKey::Shl(int_flags_bits(*flags), *lhs, *rhs),
        LShr { exact, lhs, rhs } => ExprKey::LShr(*exact, *lhs, *rhs),
        AShr { exact, lhs, rhs } => ExprKey::AShr(*exact, *lhs, *rhs),
        ICmp { pred, lhs, rhs } => {
            if matches!(pred, IntPredicate::Eq | IntPredicate::Ne) {
                let (l, r) = order_pair(*lhs, *rhs);
                ExprKey::ICmp(int_pred_bits(*pred), l, r)
            } else {
                ExprKey::ICmp(int_pred_bits(*pred), *lhs, *rhs)
            }
        }
        Select {
            cond,
            then_val,
            else_val,
        } => ExprKey::Select(*cond, *then_val, *else_val),
        Trunc { val, to } => ExprKey::Trunc(*val, *to),
        ZExt { val, to } => ExprKey::ZExt(*val, *to),
        SExt { val, to } => ExprKey::SExt(*val, *to),
        BitCast { val, to } => ExprKey::BitCast(*val, *to),
        PtrToInt { val, to } => ExprKey::PtrToInt(*val, *to),
        IntToPtr { val, to } => ExprKey::IntToPtr(*val, *to),
        FPTrunc { val, to } => ExprKey::FPTrunc(*val, *to),
        FPExt { val, to } => ExprKey::FPExt(*val, *to),
        FPToUI { val, to } => ExprKey::FPToUI(*val, *to),
        FPToSI { val, to } => ExprKey::FPToSI(*val, *to),
        UIToFP { val, to } => ExprKey::UIToFP(*val, *to),
        SIToFP { val, to } => ExprKey::SIToFP(*val, *to),
        AddrSpaceCast { val, to } => ExprKey::AddrSpaceCast(*val, *to),
        FAdd { flags, lhs, rhs } => ExprKey::FAdd(fast_math_bits(*flags), *lhs, *rhs),
        FSub { flags, lhs, rhs } => ExprKey::FSub(fast_math_bits(*flags), *lhs, *rhs),
        FMul { flags, lhs, rhs } => ExprKey::FMul(fast_math_bits(*flags), *lhs, *rhs),
        FDiv { flags, lhs, rhs } => ExprKey::FDiv(fast_math_bits(*flags), *lhs, *rhs),
        FRem { flags, lhs, rhs } => ExprKey::FRem(fast_math_bits(*flags), *lhs, *rhs),
        FNeg { flags, operand } => ExprKey::FNeg(fast_math_bits(*flags), *operand),
        FCmp {
            flags,
            pred,
            lhs,
            rhs,
        } => ExprKey::FCmp(fast_math_bits(*flags), float_pred_bits(*pred), *lhs, *rhs),
        _ => return None,
    })
}

/// Global Value Numbering pass.
pub struct Gvn;

impl FunctionPass for Gvn {
    fn name(&self) -> &'static str {
        "gvn"
    }

    fn run_on_function(&mut self, _ctx: &mut Context, func: &mut Function) -> bool {
        if func.blocks.is_empty() {
            return false;
        }

        let cfg = Cfg::compute(func);
        let dom = DomTree::compute(func, &cfg);

        let mut dom_children: Vec<Vec<BlockId>> = vec![Vec::new(); func.num_blocks()];
        for bi in 0..func.num_blocks() {
            let bid = BlockId(bi as u32);
            if let Some(idom) = dom.idom(bid) {
                dom_children[idom.0 as usize].push(bid);
            }
        }

        let mut subst: HashMap<InstrId, ValueRef> = HashMap::new();
        let mut remove: HashSet<InstrId> = HashSet::new();

        rewrite_block(
            func,
            BlockId(0),
            &dom_children,
            &mut HashMap::new(),
            &mut HashMap::new(),
            &mut subst,
            &mut remove,
        );

        if subst.is_empty() {
            return false;
        }

        for instr in &mut func.instructions {
            instr.kind = subst_kind(instr.kind.clone(), &subst);
        }

        for bb in &mut func.blocks {
            bb.body.retain(|iid| !remove.contains(iid));
            if let Some(tid) = bb.terminator {
                bb.terminator = Some(tid);
            }
        }

        true
    }
}

#[allow(clippy::too_many_arguments)]
fn rewrite_block(
    func: &mut Function,
    bid: BlockId,
    dom_children: &[Vec<BlockId>],
    exprs_in: &mut HashMap<ExprKey, ValueRef>,
    loads_in: &mut HashMap<ValueRef, ValueRef>,
    subst: &mut HashMap<InstrId, ValueRef>,
    remove: &mut HashSet<InstrId>,
) {
    let mut exprs = exprs_in.clone();
    let mut loads = loads_in.clone();

    let body = func.blocks[bid.0 as usize].body.clone();
    for iid in body {
        let rewritten = subst_kind(func.instr(iid).kind.clone(), subst);
        func.instr_mut(iid).kind = rewritten.clone();

        match &rewritten {
            InstrKind::Load {
                ptr,
                volatile: false,
                ..
            } => {
                if let Some(existing) = loads.get(ptr).copied() {
                    subst.insert(iid, existing);
                    remove.insert(iid);
                } else {
                    loads.insert(*ptr, ValueRef::Instruction(iid));
                }
            }
            InstrKind::Store { .. }
            | InstrKind::Call { .. }
            | InstrKind::Alloca { .. }
            | InstrKind::GetElementPtr { .. }
            | InstrKind::IntToPtr { .. }
            | InstrKind::PtrToInt { .. } => {
                loads.clear();
                if let Some(key) = expr_key(&rewritten) {
                    if let Some(existing) = exprs.get(&key).copied() {
                        subst.insert(iid, existing);
                        remove.insert(iid);
                    } else {
                        exprs.insert(key, ValueRef::Instruction(iid));
                    }
                }
            }
            _ => {
                if let Some(key) = expr_key(&rewritten) {
                    if let Some(existing) = exprs.get(&key).copied() {
                        subst.insert(iid, existing);
                        remove.insert(iid);
                    } else {
                        exprs.insert(key, ValueRef::Instruction(iid));
                    }
                }
            }
        }
    }

    if let Some(tid) = func.blocks[bid.0 as usize].terminator {
        let tk = subst_kind(func.instr(tid).kind.clone(), subst);
        func.instr_mut(tid).kind = tk;
    }

    for &child in &dom_children[bid.0 as usize] {
        rewrite_block(func, child, dom_children, &mut exprs, &mut loads, subst, remove);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use llvm_ir::{Builder, Linkage, Module};

    fn run_gvn(mut ctx: Context, mut module: Module) -> Function {
        let mut pass = Gvn;
        let changed = pass.run_on_function(&mut ctx, &mut module.functions[0]);
        assert!(changed, "GVN should change this test case");
        module.functions.remove(0)
    }

    fn make_binop_fn(kind: &str, commuted_second: bool) -> (Context, Module) {
        let mut ctx = Context::new();
        let mut module = Module::new("m");
        let mut b = Builder::new(&mut ctx, &mut module);
        b.add_function(
            "f",
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
        let x = match kind {
            "add" => b.build_add("x", a, bv),
            "sub" => b.build_sub("x", a, bv),
            "mul" => b.build_mul("x", a, bv),
            _ => unreachable!(),
        };
        let y = match kind {
            "add" => {
                if commuted_second {
                    b.build_add("y", bv, a)
                } else {
                    b.build_add("y", a, bv)
                }
            }
            "sub" => {
                if commuted_second {
                    b.build_sub("y", bv, a)
                } else {
                    b.build_sub("y", a, bv)
                }
            }
            "mul" => {
                if commuted_second {
                    b.build_mul("y", bv, a)
                } else {
                    b.build_mul("y", a, bv)
                }
            }
            _ => unreachable!(),
        };
        let s = b.build_add("s", x, y);
        b.build_ret(s);
        (ctx, module)
    }

    #[test]
    fn gvn_eliminates_same_add_in_block() {
        let (ctx, module) = make_binop_fn("add", false);
        let f = run_gvn(ctx, module);
        assert_eq!(f.blocks[0].body.len(), 2);
    }

    #[test]
    fn gvn_eliminates_commutative_add() {
        let (ctx, module) = make_binop_fn("add", true);
        let f = run_gvn(ctx, module);
        assert_eq!(f.blocks[0].body.len(), 2);
    }

    #[test]
    fn gvn_does_not_eliminate_non_commutative_sub() {
        let (mut ctx, mut module) = make_binop_fn("sub", true);
        let mut pass = Gvn;
        let changed = pass.run_on_function(&mut ctx, &mut module.functions[0]);
        assert!(!changed, "sub(a,b) and sub(b,a) are not equivalent");
    }

    #[test]
    fn gvn_eliminates_commutative_mul() {
        let (ctx, module) = make_binop_fn("mul", true);
        let f = run_gvn(ctx, module);
        assert_eq!(f.blocks[0].body.len(), 2);
    }

    #[test]
    fn gvn_eliminates_load_without_store() {
        let mut ctx = Context::new();
        let mut module = Module::new("m");
        let mut b = Builder::new(&mut ctx, &mut module);
        b.add_function("f", b.ctx.i32_ty, vec![], vec![], false, Linkage::External);
        let entry = b.add_block("entry");
        b.position_at_end(entry);
        let p = b.build_alloca("p", b.ctx.i32_ty);
        let c = b.const_int(b.ctx.i32_ty, 9);
        b.build_store(c, p);
        let l1 = b.build_load("l1", b.ctx.i32_ty, p);
        let l2 = b.build_load("l2", b.ctx.i32_ty, p);
        let s = b.build_add("s", l1, l2);
        b.build_ret(s);

        let f = run_gvn(ctx, module);
        assert!(f.blocks[0].body.len() < 5);
    }

    #[test]
    fn gvn_store_invalidates_load_value_numbering() {
        let mut ctx = Context::new();
        let mut module = Module::new("m");
        let mut b = Builder::new(&mut ctx, &mut module);
        b.add_function("f", b.ctx.i32_ty, vec![], vec![], false, Linkage::External);
        let entry = b.add_block("entry");
        b.position_at_end(entry);
        let p = b.build_alloca("p", b.ctx.i32_ty);
        let c1 = b.const_int(b.ctx.i32_ty, 1);
        b.build_store(c1, p);
        let _l1 = b.build_load("l1", b.ctx.i32_ty, p);
        let c2 = b.const_int(b.ctx.i32_ty, 2);
        b.build_store(c2, p);
        let l2 = b.build_load("l2", b.ctx.i32_ty, p);
        b.build_ret(l2);

        let mut pass = Gvn;
        let mut changed = false;
        changed |= pass.run_on_function(&mut ctx, &mut module.functions[0]);
        assert!(!changed, "second load must not be replaced across store");
    }

    #[test]
    fn gvn_eliminates_redundant_icmp_eq_commuted() {
        let mut ctx = Context::new();
        let mut module = Module::new("m");
        let mut b = Builder::new(&mut ctx, &mut module);
        b.add_function(
            "f",
            b.ctx.i1_ty,
            vec![b.ctx.i64_ty, b.ctx.i64_ty],
            vec!["a".into(), "b".into()],
            false,
            Linkage::External,
        );
        let entry = b.add_block("entry");
        b.position_at_end(entry);
        let a = b.get_arg(0);
        let bv = b.get_arg(1);
        let c1 = b.build_icmp("c1", IntPredicate::Eq, a, bv);
        let c2 = b.build_icmp("c2", IntPredicate::Eq, bv, a);
        let r = b.build_and("r", c1, c2);
        b.build_ret(r);

        let f = run_gvn(ctx, module);
        assert_eq!(f.blocks[0].body.len(), 2);
    }

    #[test]
    fn gvn_eliminates_cross_block_when_dominated() {
        let mut ctx = Context::new();
        let mut module = Module::new("m");
        let mut b = Builder::new(&mut ctx, &mut module);
        b.add_function(
            "f",
            b.ctx.i64_ty,
            vec![b.ctx.i64_ty, b.ctx.i64_ty, b.ctx.i1_ty],
            vec!["a".into(), "b".into(), "cond".into()],
            false,
            Linkage::External,
        );
        let entry = b.add_block("entry");
        let then_bb = b.add_block("then");
        let else_bb = b.add_block("else");
        let merge = b.add_block("merge");

        b.position_at_end(entry);
        let a = b.get_arg(0);
        let bv = b.get_arg(1);
        let cond = b.get_arg(2);
        let x = b.build_add("x", a, bv);
        b.build_cond_br(cond, then_bb, else_bb);

        b.position_at_end(then_bb);
        let y = b.build_add("y", a, bv);
        b.build_br(merge);

        b.position_at_end(else_bb);
        b.build_br(merge);

        b.position_at_end(merge);
        let p = b.build_phi("p", b.ctx.i64_ty, vec![(y, then_bb), (x, else_bb)]);
        b.build_ret(p);

        let f = run_gvn(ctx, module);
        assert!(f.blocks[1].body.is_empty());
    }

    #[test]
    fn gvn_does_not_cross_non_dominating_siblings() {
        let mut ctx = Context::new();
        let mut module = Module::new("m");
        let mut b = Builder::new(&mut ctx, &mut module);
        b.add_function(
            "f",
            b.ctx.i64_ty,
            vec![b.ctx.i64_ty, b.ctx.i64_ty, b.ctx.i1_ty],
            vec!["a".into(), "b".into(), "cond".into()],
            false,
            Linkage::External,
        );
        let entry = b.add_block("entry");
        let then_bb = b.add_block("then");
        let else_bb = b.add_block("else");
        let merge = b.add_block("merge");

        b.position_at_end(entry);
        let a = b.get_arg(0);
        let bv = b.get_arg(1);
        let cond = b.get_arg(2);
        b.build_cond_br(cond, then_bb, else_bb);

        b.position_at_end(then_bb);
        let t = b.build_add("t", a, bv);
        b.build_br(merge);

        b.position_at_end(else_bb);
        let e = b.build_add("e", a, bv);
        b.build_br(merge);

        b.position_at_end(merge);
        let p = b.build_phi("p", b.ctx.i64_ty, vec![(t, then_bb), (e, else_bb)]);
        b.build_ret(p);

        let mut pass = Gvn;
        let changed = pass.run_on_function(&mut ctx, &mut module.functions[0]);
        assert!(!changed, "sibling-block expressions are not dominance-equivalent");
    }

    #[test]
    fn gvn_eliminates_redundant_select() {
        let mut ctx = Context::new();
        let mut module = Module::new("m");
        let mut b = Builder::new(&mut ctx, &mut module);
        b.add_function(
            "f",
            b.ctx.i64_ty,
            vec![b.ctx.i1_ty, b.ctx.i64_ty, b.ctx.i64_ty],
            vec!["c".into(), "a".into(), "b".into()],
            false,
            Linkage::External,
        );
        let entry = b.add_block("entry");
        b.position_at_end(entry);
        let c = b.get_arg(0);
        let a = b.get_arg(1);
        let bv = b.get_arg(2);
        let s1 = b.build_select("s1", c, a, bv);
        let s2 = b.build_select("s2", c, a, bv);
        let r = b.build_add("r", s1, s2);
        b.build_ret(r);

        let f = run_gvn(ctx, module);
        assert_eq!(f.blocks[0].body.len(), 2);
    }
}
