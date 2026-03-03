//! Constant folding over SSA values.
//!
//! `try_fold` attempts to evaluate a single instruction to a compile-time
//! constant.  It covers integer arithmetic, bitwise ops, shifts, integer
//! comparisons, and `select` with a constant condition.
//!
//! Floating-point, memory, and side-effecting instructions are never folded.
//! The function is pure: it only reads `ctx` and `kind`; new constants are
//! allocated with `ctx.const_int`.

use llvm_ir::{ConstId, ConstantData, Context, IntPredicate, InstrKind, ValueRef};

/// Try to constant-fold `kind`.
///
/// Returns `Some(cid)` — a `ConstId` for the folded result — when every
/// operand is a `ValueRef::Constant` pointing to a `ConstantData::Int`.
/// Returns `None` if any operand is non-constant or if the operation is
/// not foldable (e.g. division by zero, non-integer operands).
pub fn try_fold(ctx: &mut Context, kind: &InstrKind) -> Option<ConstId> {
    match kind {
        // --- Binary integer arithmetic ---
        InstrKind::Add { lhs, rhs, .. } => {
            let (ty, l) = const_int(ctx, *lhs)?;
            let (_, r)  = const_int(ctx, *rhs)?;
            Some(ctx.const_int(ty, l.wrapping_add(r)))
        }
        InstrKind::Sub { lhs, rhs, .. } => {
            let (ty, l) = const_int(ctx, *lhs)?;
            let (_, r)  = const_int(ctx, *rhs)?;
            Some(ctx.const_int(ty, l.wrapping_sub(r)))
        }
        InstrKind::Mul { lhs, rhs, .. } => {
            let (ty, l) = const_int(ctx, *lhs)?;
            let (_, r)  = const_int(ctx, *rhs)?;
            Some(ctx.const_int(ty, l.wrapping_mul(r)))
        }
        InstrKind::UDiv { lhs, rhs, .. } => {
            let (ty, l) = const_int(ctx, *lhs)?;
            let (_, r)  = const_int(ctx, *rhs)?;
            if r == 0 { return None; }
            Some(ctx.const_int(ty, l / r))
        }
        InstrKind::SDiv { lhs, rhs, .. } => {
            let (ty, l) = const_int(ctx, *lhs)?;
            let (_, r)  = const_int(ctx, *rhs)?;
            if r == 0 { return None; }
            Some(ctx.const_int(ty, (l as i64).wrapping_div(r as i64) as u64))
        }
        InstrKind::URem { lhs, rhs } => {
            let (ty, l) = const_int(ctx, *lhs)?;
            let (_, r)  = const_int(ctx, *rhs)?;
            if r == 0 { return None; }
            Some(ctx.const_int(ty, l % r))
        }
        InstrKind::SRem { lhs, rhs } => {
            let (ty, l) = const_int(ctx, *lhs)?;
            let (_, r)  = const_int(ctx, *rhs)?;
            if r == 0 { return None; }
            Some(ctx.const_int(ty, (l as i64).wrapping_rem(r as i64) as u64))
        }

        // --- Bitwise ---
        InstrKind::And { lhs, rhs } => {
            let (ty, l) = const_int(ctx, *lhs)?;
            let (_, r)  = const_int(ctx, *rhs)?;
            Some(ctx.const_int(ty, l & r))
        }
        InstrKind::Or { lhs, rhs } => {
            let (ty, l) = const_int(ctx, *lhs)?;
            let (_, r)  = const_int(ctx, *rhs)?;
            Some(ctx.const_int(ty, l | r))
        }
        InstrKind::Xor { lhs, rhs } => {
            let (ty, l) = const_int(ctx, *lhs)?;
            let (_, r)  = const_int(ctx, *rhs)?;
            Some(ctx.const_int(ty, l ^ r))
        }

        // --- Shifts (clamp shift amount to 0–63) ---
        InstrKind::Shl { lhs, rhs, .. } => {
            let (ty, l) = const_int(ctx, *lhs)?;
            let (_, r)  = const_int(ctx, *rhs)?;
            Some(ctx.const_int(ty, l << (r & 63)))
        }
        InstrKind::LShr { lhs, rhs, .. } => {
            let (ty, l) = const_int(ctx, *lhs)?;
            let (_, r)  = const_int(ctx, *rhs)?;
            Some(ctx.const_int(ty, l >> (r & 63)))
        }
        InstrKind::AShr { lhs, rhs, .. } => {
            let (ty, l) = const_int(ctx, *lhs)?;
            let (_, r)  = const_int(ctx, *rhs)?;
            Some(ctx.const_int(ty, ((l as i64) >> ((r & 63) as u32)) as u64))
        }

        // --- Integer comparison → i1 result ---
        InstrKind::ICmp { pred, lhs, rhs } => {
            let (_, l) = const_int(ctx, *lhs)?;
            let (_, r) = const_int(ctx, *rhs)?;
            let result = icmp_eval(*pred, l, r) as u64;
            Some(ctx.const_int(ctx.i1_ty, result))
        }

        // --- Select with constant condition ---
        InstrKind::Select { cond, then_val, else_val } => {
            let (_, c) = const_int(ctx, *cond)?;
            let chosen = if c != 0 { then_val } else { else_val };
            match chosen {
                ValueRef::Constant(cid) => Some(*cid),
                _ => None,
            }
        }

        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Extract `(TypeId, u64)` from a `ValueRef::Constant` pointing to a
/// `ConstantData::Int`.  Returns `None` for any other variant.
pub(crate) fn const_int(ctx: &Context, vref: ValueRef) -> Option<(llvm_ir::TypeId, u64)> {
    let cid = match vref {
        ValueRef::Constant(id) => id,
        _ => return None,
    };
    match ctx.get_const(cid) {
        ConstantData::Int { ty, val } => Some((*ty, *val)),
        _ => None,
    }
}

fn icmp_eval(pred: IntPredicate, l: u64, r: u64) -> bool {
    match pred {
        IntPredicate::Eq  => l == r,
        IntPredicate::Ne  => l != r,
        IntPredicate::Ugt => l >  r,
        IntPredicate::Uge => l >= r,
        IntPredicate::Ult => l <  r,
        IntPredicate::Ule => l <= r,
        IntPredicate::Sgt => (l as i64) >  (r as i64),
        IntPredicate::Sge => (l as i64) >= (r as i64),
        IntPredicate::Slt => (l as i64) <  (r as i64),
        IntPredicate::Sle => (l as i64) <= (r as i64),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use llvm_ir::{Context, IntArithFlags, InstrKind};

    fn c(ctx: &mut Context, v: u64) -> ValueRef {
        ValueRef::Constant(ctx.const_int(ctx.i32_ty, v))
    }

    #[test]
    fn fold_add() {
        let mut ctx = Context::new();
        let kind = InstrKind::Add {
            flags: IntArithFlags::default(),
            lhs: c(&mut ctx, 3),
            rhs: c(&mut ctx, 4),
        };
        let result = try_fold(&mut ctx, &kind).unwrap();
        assert_eq!(ctx.get_const(result), &ConstantData::Int { ty: ctx.i32_ty, val: 7 });
    }

    #[test]
    fn fold_sub_wrapping() {
        let mut ctx = Context::new();
        let kind = InstrKind::Sub {
            flags: IntArithFlags::default(),
            lhs: c(&mut ctx, 2),
            rhs: c(&mut ctx, 5),
        };
        let result = try_fold(&mut ctx, &kind).unwrap();
        // 2u64 - 5u64 wraps
        if let ConstantData::Int { val, .. } = ctx.get_const(result) {
            assert_eq!(*val, 2u64.wrapping_sub(5));
        } else {
            panic!("expected Int constant");
        }
    }

    #[test]
    fn fold_udiv_by_zero_returns_none() {
        let mut ctx = Context::new();
        let kind = InstrKind::UDiv {
            exact: false,
            lhs: c(&mut ctx, 10),
            rhs: c(&mut ctx, 0),
        };
        assert!(try_fold(&mut ctx, &kind).is_none());
    }

    #[test]
    fn fold_icmp_eq() {
        let mut ctx = Context::new();
        let kind = InstrKind::ICmp {
            pred: IntPredicate::Eq,
            lhs: c(&mut ctx, 7),
            rhs: c(&mut ctx, 7),
        };
        let result = try_fold(&mut ctx, &kind).unwrap();
        assert_eq!(ctx.get_const(result), &ConstantData::Int { ty: ctx.i1_ty, val: 1 });
    }

    #[test]
    fn fold_icmp_slt() {
        let mut ctx = Context::new();
        // -1 <s 0  →  true
        let neg1 = c(&mut ctx, u64::MAX);
        let zero = c(&mut ctx, 0);
        let kind = InstrKind::ICmp { pred: IntPredicate::Slt, lhs: neg1, rhs: zero };
        let result = try_fold(&mut ctx, &kind).unwrap();
        assert_eq!(ctx.get_const(result), &ConstantData::Int { ty: ctx.i1_ty, val: 1 });
    }

    #[test]
    fn fold_select_constant_cond() {
        let mut ctx = Context::new();
        let cond_true  = ValueRef::Constant(ctx.const_int(ctx.i1_ty, 1));
        let cond_false = ValueRef::Constant(ctx.const_int(ctx.i1_ty, 0));
        let then_c = c(&mut ctx, 42);
        let else_c = c(&mut ctx, 99);

        let k1 = InstrKind::Select { cond: cond_true,  then_val: then_c, else_val: else_c };
        let k2 = InstrKind::Select { cond: cond_false, then_val: then_c, else_val: else_c };

        let r1 = try_fold(&mut ctx, &k1).unwrap();
        let r2 = try_fold(&mut ctx, &k2).unwrap();
        assert_eq!(ctx.get_const(r1), &ConstantData::Int { ty: ctx.i32_ty, val: 42 });
        assert_eq!(ctx.get_const(r2), &ConstantData::Int { ty: ctx.i32_ty, val: 99 });
    }

    #[test]
    fn non_constant_operand_returns_none() {
        let mut ctx = Context::new();
        let arg = ValueRef::Argument(llvm_ir::ArgId(0));
        let kind = InstrKind::Add {
            flags: IntArithFlags::default(),
            lhs: arg,
            rhs: c(&mut ctx, 1),
        };
        assert!(try_fold(&mut ctx, &kind).is_none());
    }
}
