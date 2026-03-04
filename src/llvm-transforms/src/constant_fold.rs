//! Constant folding over SSA values.
//!
//! `try_fold` attempts to evaluate a single instruction to a compile-time
//! constant.  It covers integer arithmetic, bitwise ops, shifts, integer
//! comparisons, and `select` with a constant condition.
//!
//! Floating-point, memory, and side-effecting instructions are never folded.
//! The function is pure: it only reads `ctx` and `kind`; new constants are
//! allocated with `ctx.const_int`.

use llvm_ir::{ConstId, ConstantData, Context, InstrKind, IntPredicate, ValueRef};

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
            let (_, r) = const_int(ctx, *rhs)?;
            Some(ctx.const_int(ty, l.wrapping_add(r)))
        }
        InstrKind::Sub { lhs, rhs, .. } => {
            let (ty, l) = const_int(ctx, *lhs)?;
            let (_, r) = const_int(ctx, *rhs)?;
            Some(ctx.const_int(ty, l.wrapping_sub(r)))
        }
        InstrKind::Mul { lhs, rhs, .. } => {
            let (ty, l) = const_int(ctx, *lhs)?;
            let (_, r) = const_int(ctx, *rhs)?;
            Some(ctx.const_int(ty, l.wrapping_mul(r)))
        }
        InstrKind::UDiv { lhs, rhs, .. } => {
            let (ty, l) = const_int(ctx, *lhs)?;
            let (_, r) = const_int(ctx, *rhs)?;
            if r == 0 {
                return None;
            }
            Some(ctx.const_int(ty, l / r))
        }
        InstrKind::SDiv { lhs, rhs, .. } => {
            let (ty, l) = const_int(ctx, *lhs)?;
            let (_, r) = const_int(ctx, *rhs)?;
            let bits = int_bit_width(ctx, ty)?;
            let sl = sign_extend(l, bits);
            let sr = sign_extend(r, bits);
            if sr == 0 {
                return None;
            }
            // MIN_INT / -1 overflows → poison; return None.
            let type_min: i64 = if bits >= 64 {
                i64::MIN
            } else {
                -(1i64 << (bits - 1))
            };
            if sl == type_min && sr == -1 {
                return None;
            }
            Some(ctx.const_int(ty, trunc_bits(sl.wrapping_div(sr) as u64, bits)))
        }
        InstrKind::URem { lhs, rhs } => {
            let (ty, l) = const_int(ctx, *lhs)?;
            let (_, r) = const_int(ctx, *rhs)?;
            if r == 0 {
                return None;
            }
            Some(ctx.const_int(ty, l % r))
        }
        InstrKind::SRem { lhs, rhs } => {
            let (ty, l) = const_int(ctx, *lhs)?;
            let (_, r) = const_int(ctx, *rhs)?;
            let bits = int_bit_width(ctx, ty)?;
            let sl = sign_extend(l, bits);
            let sr = sign_extend(r, bits);
            if sr == 0 {
                return None;
            }
            Some(ctx.const_int(ty, trunc_bits(sl.wrapping_rem(sr) as u64, bits)))
        }

        // --- Bitwise ---
        InstrKind::And { lhs, rhs } => {
            let (ty, l) = const_int(ctx, *lhs)?;
            let (_, r) = const_int(ctx, *rhs)?;
            Some(ctx.const_int(ty, l & r))
        }
        InstrKind::Or { lhs, rhs } => {
            let (ty, l) = const_int(ctx, *lhs)?;
            let (_, r) = const_int(ctx, *rhs)?;
            Some(ctx.const_int(ty, l | r))
        }
        InstrKind::Xor { lhs, rhs } => {
            let (ty, l) = const_int(ctx, *lhs)?;
            let (_, r) = const_int(ctx, *rhs)?;
            Some(ctx.const_int(ty, l ^ r))
        }

        // --- Shifts (mask shift amount to bit_width - 1, per LLVM semantics) ---
        InstrKind::Shl { lhs, rhs, .. } => {
            let (ty, l) = const_int(ctx, *lhs)?;
            let (_, r) = const_int(ctx, *rhs)?;
            let mask = shift_mask(ctx, ty)?;
            Some(ctx.const_int(ty, l << (r & mask)))
        }
        InstrKind::LShr { lhs, rhs, .. } => {
            let (ty, l) = const_int(ctx, *lhs)?;
            let (_, r) = const_int(ctx, *rhs)?;
            let mask = shift_mask(ctx, ty)?;
            Some(ctx.const_int(ty, l >> (r & mask)))
        }
        InstrKind::AShr { lhs, rhs, .. } => {
            let (ty, l) = const_int(ctx, *lhs)?;
            let (_, r) = const_int(ctx, *rhs)?;
            let mask = shift_mask(ctx, ty)?;
            let bits = int_bit_width(ctx, ty)?;
            let sl = sign_extend(l, bits);
            Some(ctx.const_int(ty, trunc_bits((sl >> ((r & mask) as u32)) as u64, bits)))
        }

        // --- Integer comparison → i1 result ---
        InstrKind::ICmp { pred, lhs, rhs } => {
            let (ty, l) = const_int(ctx, *lhs)?;
            let (_, r) = const_int(ctx, *rhs)?;
            let bits = int_bit_width(ctx, ty)?;
            let result = icmp_eval(*pred, l, r, bits) as u64;
            Some(ctx.const_int(ctx.i1_ty, result))
        }

        // --- Select with constant condition ---
        InstrKind::Select {
            cond,
            then_val,
            else_val,
        } => {
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

/// Sign-extend a value from `bits` width to a full `i64`.
///
/// Constants for iN types are stored as their unsigned bit pattern in the
/// low `bits` of a `u64`.  For signed operations we must first recover the
/// true signed value.  For example, i8 value `0xFF` = 255u64 → -1i64.
fn sign_extend(val: u64, bits: u32) -> i64 {
    debug_assert!(bits > 0 && bits <= 64);
    let shift = 64u32.saturating_sub(bits);
    ((val << shift) as i64) >> shift
}

/// Truncate a `u64` to the low `bits` bits.
///
/// Used to bring signed-operation results back to the canonical `u64`
/// storage format (no bits above position `bits-1` set).
fn trunc_bits(val: u64, bits: u32) -> u64 {
    if bits >= 64 {
        val
    } else {
        val & ((1u64 << bits).wrapping_sub(1))
    }
}

/// Extract the integer bit width from a `TypeId`.  Returns `None` for
/// non-integer types (Float, Pointer, etc.).
fn int_bit_width(ctx: &Context, ty: llvm_ir::TypeId) -> Option<u32> {
    if let llvm_ir::TypeData::Integer(bits) = ctx.get_type(ty) {
        Some(*bits)
    } else {
        None
    }
}

/// Returns the shift-amount mask for an integer type: `bit_width - 1`.
///
/// LLVM semantics: a shift by `r` on an `iN` value uses only the low
/// `log2(N)` bits of `r` (i.e. `r & (N - 1)`).  For i32 this is 31,
/// for i8 it is 7, for i64 it is 63.  Returns `None` for non-integer types.
fn shift_mask(ctx: &Context, ty: llvm_ir::TypeId) -> Option<u64> {
    if let llvm_ir::TypeData::Integer(bits) = ctx.get_type(ty) {
        Some((*bits as u64).saturating_sub(1))
    } else {
        None
    }
}

fn icmp_eval(pred: IntPredicate, l: u64, r: u64, bits: u32) -> bool {
    match pred {
        IntPredicate::Eq => l == r,
        IntPredicate::Ne => l != r,
        IntPredicate::Ugt => l > r,
        IntPredicate::Uge => l >= r,
        IntPredicate::Ult => l < r,
        IntPredicate::Ule => l <= r,
        IntPredicate::Sgt => sign_extend(l, bits) > sign_extend(r, bits),
        IntPredicate::Sge => sign_extend(l, bits) >= sign_extend(r, bits),
        IntPredicate::Slt => sign_extend(l, bits) < sign_extend(r, bits),
        IntPredicate::Sle => sign_extend(l, bits) <= sign_extend(r, bits),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use llvm_ir::{Context, InstrKind, IntArithFlags};

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
        assert_eq!(
            ctx.get_const(result),
            &ConstantData::Int {
                ty: ctx.i32_ty,
                val: 7
            }
        );
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
        assert_eq!(
            ctx.get_const(result),
            &ConstantData::Int {
                ty: ctx.i1_ty,
                val: 1
            }
        );
    }

    #[test]
    fn fold_icmp_slt() {
        let mut ctx = Context::new();
        // -1 <s 0  →  true
        let neg1 = c(&mut ctx, u64::MAX);
        let zero = c(&mut ctx, 0);
        let kind = InstrKind::ICmp {
            pred: IntPredicate::Slt,
            lhs: neg1,
            rhs: zero,
        };
        let result = try_fold(&mut ctx, &kind).unwrap();
        assert_eq!(
            ctx.get_const(result),
            &ConstantData::Int {
                ty: ctx.i1_ty,
                val: 1
            }
        );
    }

    #[test]
    fn fold_select_constant_cond() {
        let mut ctx = Context::new();
        let cond_true = ValueRef::Constant(ctx.const_int(ctx.i1_ty, 1));
        let cond_false = ValueRef::Constant(ctx.const_int(ctx.i1_ty, 0));
        let then_c = c(&mut ctx, 42);
        let else_c = c(&mut ctx, 99);

        let k1 = InstrKind::Select {
            cond: cond_true,
            then_val: then_c,
            else_val: else_c,
        };
        let k2 = InstrKind::Select {
            cond: cond_false,
            then_val: then_c,
            else_val: else_c,
        };

        let r1 = try_fold(&mut ctx, &k1).unwrap();
        let r2 = try_fold(&mut ctx, &k2).unwrap();
        assert_eq!(
            ctx.get_const(r1),
            &ConstantData::Int {
                ty: ctx.i32_ty,
                val: 42
            }
        );
        assert_eq!(
            ctx.get_const(r2),
            &ConstantData::Int {
                ty: ctx.i32_ty,
                val: 99
            }
        );
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

    // Helpers for shift tests using i8 and i32.
    fn c8(ctx: &mut Context, v: u64) -> ValueRef {
        ValueRef::Constant(ctx.const_int(ctx.i8_ty, v))
    }

    #[test]
    fn shl_i32_mask_is_31() {
        // i32 shl 1, 32 → shift amount 32 & 31 = 0 → result = 1
        // (shift >= bit_width is poison in LLVM; folding as 0-shift is the
        // safe choice here — the important thing is we don't use mask=63)
        let mut ctx = Context::new();
        let kind = InstrKind::Shl {
            flags: llvm_ir::IntArithFlags::default(),
            lhs: c(&mut ctx, 1),
            rhs: c(&mut ctx, 32), // 32 & 31 == 0
        };
        let result = try_fold(&mut ctx, &kind).unwrap();
        assert_eq!(
            ctx.get_const(result),
            &ConstantData::Int {
                ty: ctx.i32_ty,
                val: 1
            }
        ); // 1 << 0 = 1
    }

    #[test]
    fn shl_i32_normal() {
        // i32 shl 1, 4 → 16
        let mut ctx = Context::new();
        let kind = InstrKind::Shl {
            flags: llvm_ir::IntArithFlags::default(),
            lhs: c(&mut ctx, 1),
            rhs: c(&mut ctx, 4),
        };
        let result = try_fold(&mut ctx, &kind).unwrap();
        assert_eq!(
            ctx.get_const(result),
            &ConstantData::Int {
                ty: ctx.i32_ty,
                val: 16
            }
        );
    }

    #[test]
    fn shl_i8_mask_is_7() {
        // i8 shl 1, 8 → shift amount 8 & 7 = 0 → result = 1
        let mut ctx = Context::new();
        let kind = InstrKind::Shl {
            flags: llvm_ir::IntArithFlags::default(),
            lhs: c8(&mut ctx, 1),
            rhs: c8(&mut ctx, 8), // 8 & 7 == 0
        };
        let result = try_fold(&mut ctx, &kind).unwrap();
        assert_eq!(
            ctx.get_const(result),
            &ConstantData::Int {
                ty: ctx.i8_ty,
                val: 1
            }
        ); // 1 << 0 = 1
    }

    #[test]
    fn lshr_i32_mask_is_31() {
        // i32 lshr 0x8000_0000, 31 → 1
        let mut ctx = Context::new();
        let kind = InstrKind::LShr {
            exact: false,
            lhs: c(&mut ctx, 0x8000_0000),
            rhs: c(&mut ctx, 31),
        };
        let result = try_fold(&mut ctx, &kind).unwrap();
        assert_eq!(
            ctx.get_const(result),
            &ConstantData::Int {
                ty: ctx.i32_ty,
                val: 1
            }
        );
    }

    #[test]
    fn ashr_i8_sign_extends() {
        // i8 ashr 0x80 (-128 as i8), 7 → 0xFF (-1 as i8) stored as 255u64
        let mut ctx = Context::new();
        let kind = InstrKind::AShr {
            exact: false,
            lhs: c8(&mut ctx, 0x80),
            rhs: c8(&mut ctx, 7),
        };
        let result = try_fold(&mut ctx, &kind).unwrap();
        assert_eq!(
            ctx.get_const(result),
            &ConstantData::Int {
                ty: ctx.i8_ty,
                val: 0xFF
            }
        ); // -1 as i8
    }

    #[test]
    fn sdiv_i8_signs() {
        // i8 sdiv -1, 2 → 0  (truncates toward zero)
        // -1 stored as 0xFF = 255u64; 2 stored as 2
        let mut ctx = Context::new();
        let kind = InstrKind::SDiv {
            exact: false,
            lhs: c8(&mut ctx, 0xFF), // -1
            rhs: c8(&mut ctx, 2),
        };
        let result = try_fold(&mut ctx, &kind).unwrap();
        assert_eq!(
            ctx.get_const(result),
            &ConstantData::Int {
                ty: ctx.i8_ty,
                val: 0
            }
        ); // -1 / 2 = 0
    }

    #[test]
    fn sdiv_i8_negative_by_negative() {
        // i8 sdiv -4, -2 → 2
        let mut ctx = Context::new();
        let kind = InstrKind::SDiv {
            exact: false,
            lhs: c8(&mut ctx, 0xFC), // -4
            rhs: c8(&mut ctx, 0xFE), // -2
        };
        let result = try_fold(&mut ctx, &kind).unwrap();
        assert_eq!(
            ctx.get_const(result),
            &ConstantData::Int {
                ty: ctx.i8_ty,
                val: 2
            }
        );
    }

    #[test]
    fn sdiv_overflow_returns_none() {
        // i8 sdiv MIN_INT(-128), -1 → overflow → None
        let mut ctx = Context::new();
        let kind = InstrKind::SDiv {
            exact: false,
            lhs: c8(&mut ctx, 0x80), // -128
            rhs: c8(&mut ctx, 0xFF), // -1
        };
        assert!(try_fold(&mut ctx, &kind).is_none());
    }

    #[test]
    fn srem_i8_negative() {
        // i8 srem -7, 3 → -1  (sign follows dividend)
        let mut ctx = Context::new();
        let kind = InstrKind::SRem {
            lhs: c8(&mut ctx, 0xF9), // -7
            rhs: c8(&mut ctx, 3),
        };
        let result = try_fold(&mut ctx, &kind).unwrap();
        assert_eq!(
            ctx.get_const(result),
            &ConstantData::Int {
                ty: ctx.i8_ty,
                val: 0xFF
            }
        ); // -1
    }

    #[test]
    fn icmp_slt_i8_negative() {
        // i8 icmp slt -1, 0 → true
        // -1 stored as 0xFF; without sign-extend (0xFF as i64 = 255) > 0 → wrong
        let mut ctx = Context::new();
        let kind = InstrKind::ICmp {
            pred: IntPredicate::Slt,
            lhs: c8(&mut ctx, 0xFF), // -1
            rhs: c8(&mut ctx, 0),
        };
        let result = try_fold(&mut ctx, &kind).unwrap();
        assert_eq!(
            ctx.get_const(result),
            &ConstantData::Int {
                ty: ctx.i1_ty,
                val: 1
            }
        ); // true
    }

    #[test]
    fn icmp_sgt_i32_negative() {
        // i32 icmp sgt 0, -1 → true
        // -1 stored as 0xFFFF_FFFF
        let mut ctx = Context::new();
        let kind = InstrKind::ICmp {
            pred: IntPredicate::Sgt,
            lhs: c(&mut ctx, 0),
            rhs: c(&mut ctx, 0xFFFF_FFFF), // -1 as i32
        };
        let result = try_fold(&mut ctx, &kind).unwrap();
        assert_eq!(
            ctx.get_const(result),
            &ConstantData::Int {
                ty: ctx.i1_ty,
                val: 1
            }
        ); // true
    }
}
