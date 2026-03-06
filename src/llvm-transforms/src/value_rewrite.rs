use llvm_ir::{InstrKind, ValueRef};

/// Rewrite every `ValueRef` operand appearing in `kind` via mapper `f`.
pub(crate) fn rewrite_values_in_kind<F>(kind: InstrKind, mut f: F) -> InstrKind
where
    F: FnMut(ValueRef) -> ValueRef,
{
    match kind {
        InstrKind::Add { flags, lhs, rhs } => InstrKind::Add {
            flags,
            lhs: f(lhs),
            rhs: f(rhs),
        },
        InstrKind::Sub { flags, lhs, rhs } => InstrKind::Sub {
            flags,
            lhs: f(lhs),
            rhs: f(rhs),
        },
        InstrKind::Mul { flags, lhs, rhs } => InstrKind::Mul {
            flags,
            lhs: f(lhs),
            rhs: f(rhs),
        },
        InstrKind::UDiv { exact, lhs, rhs } => InstrKind::UDiv {
            exact,
            lhs: f(lhs),
            rhs: f(rhs),
        },
        InstrKind::SDiv { exact, lhs, rhs } => InstrKind::SDiv {
            exact,
            lhs: f(lhs),
            rhs: f(rhs),
        },
        InstrKind::URem { lhs, rhs } => InstrKind::URem {
            lhs: f(lhs),
            rhs: f(rhs),
        },
        InstrKind::SRem { lhs, rhs } => InstrKind::SRem {
            lhs: f(lhs),
            rhs: f(rhs),
        },
        InstrKind::And { lhs, rhs } => InstrKind::And {
            lhs: f(lhs),
            rhs: f(rhs),
        },
        InstrKind::Or { lhs, rhs } => InstrKind::Or {
            lhs: f(lhs),
            rhs: f(rhs),
        },
        InstrKind::Xor { lhs, rhs } => InstrKind::Xor {
            lhs: f(lhs),
            rhs: f(rhs),
        },
        InstrKind::Shl { flags, lhs, rhs } => InstrKind::Shl {
            flags,
            lhs: f(lhs),
            rhs: f(rhs),
        },
        InstrKind::LShr { exact, lhs, rhs } => InstrKind::LShr {
            exact,
            lhs: f(lhs),
            rhs: f(rhs),
        },
        InstrKind::AShr { exact, lhs, rhs } => InstrKind::AShr {
            exact,
            lhs: f(lhs),
            rhs: f(rhs),
        },
        InstrKind::FAdd { flags, lhs, rhs } => InstrKind::FAdd {
            flags,
            lhs: f(lhs),
            rhs: f(rhs),
        },
        InstrKind::FSub { flags, lhs, rhs } => InstrKind::FSub {
            flags,
            lhs: f(lhs),
            rhs: f(rhs),
        },
        InstrKind::FMul { flags, lhs, rhs } => InstrKind::FMul {
            flags,
            lhs: f(lhs),
            rhs: f(rhs),
        },
        InstrKind::FDiv { flags, lhs, rhs } => InstrKind::FDiv {
            flags,
            lhs: f(lhs),
            rhs: f(rhs),
        },
        InstrKind::FRem { flags, lhs, rhs } => InstrKind::FRem {
            flags,
            lhs: f(lhs),
            rhs: f(rhs),
        },
        InstrKind::FNeg { flags, operand } => InstrKind::FNeg {
            flags,
            operand: f(operand),
        },
        InstrKind::ICmp { pred, lhs, rhs } => InstrKind::ICmp {
            pred,
            lhs: f(lhs),
            rhs: f(rhs),
        },
        InstrKind::FCmp {
            flags,
            pred,
            lhs,
            rhs,
        } => InstrKind::FCmp {
            flags,
            pred,
            lhs: f(lhs),
            rhs: f(rhs),
        },
        InstrKind::Alloca {
            alloc_ty,
            num_elements,
            align,
        } => InstrKind::Alloca {
            alloc_ty,
            num_elements: num_elements.map(&mut f),
            align,
        },
        InstrKind::Load {
            ty,
            ptr,
            align,
            volatile,
        } => InstrKind::Load {
            ty,
            ptr: f(ptr),
            align,
            volatile,
        },
        InstrKind::Store {
            val,
            ptr,
            align,
            volatile,
        } => InstrKind::Store {
            val: f(val),
            ptr: f(ptr),
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
            ptr: f(ptr),
            indices: indices.into_iter().map(f).collect(),
        },
        InstrKind::Trunc { val, to } => InstrKind::Trunc { val: f(val), to },
        InstrKind::ZExt { val, to } => InstrKind::ZExt { val: f(val), to },
        InstrKind::SExt { val, to } => InstrKind::SExt { val: f(val), to },
        InstrKind::FPTrunc { val, to } => InstrKind::FPTrunc { val: f(val), to },
        InstrKind::FPExt { val, to } => InstrKind::FPExt { val: f(val), to },
        InstrKind::FPToUI { val, to } => InstrKind::FPToUI { val: f(val), to },
        InstrKind::FPToSI { val, to } => InstrKind::FPToSI { val: f(val), to },
        InstrKind::UIToFP { val, to } => InstrKind::UIToFP { val: f(val), to },
        InstrKind::SIToFP { val, to } => InstrKind::SIToFP { val: f(val), to },
        InstrKind::PtrToInt { val, to } => InstrKind::PtrToInt { val: f(val), to },
        InstrKind::IntToPtr { val, to } => InstrKind::IntToPtr { val: f(val), to },
        InstrKind::BitCast { val, to } => InstrKind::BitCast { val: f(val), to },
        InstrKind::AddrSpaceCast { val, to } => InstrKind::AddrSpaceCast { val: f(val), to },
        InstrKind::Select {
            cond,
            then_val,
            else_val,
        } => InstrKind::Select {
            cond: f(cond),
            then_val: f(then_val),
            else_val: f(else_val),
        },
        InstrKind::Phi { ty, incoming } => InstrKind::Phi {
            ty,
            incoming: incoming.into_iter().map(|(v, b)| (f(v), b)).collect(),
        },
        InstrKind::ExtractValue { aggregate, indices } => InstrKind::ExtractValue {
            aggregate: f(aggregate),
            indices,
        },
        InstrKind::InsertValue {
            aggregate,
            val,
            indices,
        } => InstrKind::InsertValue {
            aggregate: f(aggregate),
            val: f(val),
            indices,
        },
        InstrKind::ExtractElement { vec, idx } => InstrKind::ExtractElement {
            vec: f(vec),
            idx: f(idx),
        },
        InstrKind::InsertElement { vec, val, idx } => InstrKind::InsertElement {
            vec: f(vec),
            val: f(val),
            idx: f(idx),
        },
        InstrKind::ShuffleVector { v1, v2, mask } => InstrKind::ShuffleVector {
            v1: f(v1),
            v2: f(v2),
            mask,
        },
        InstrKind::Call {
            tail,
            callee_ty,
            callee,
            args,
        } => InstrKind::Call {
            tail,
            callee_ty,
            callee: f(callee),
            args: args.into_iter().map(f).collect(),
        },
        InstrKind::Ret { val } => InstrKind::Ret { val: val.map(f) },
        InstrKind::Br { dest } => InstrKind::Br { dest },
        InstrKind::CondBr {
            cond,
            then_dest,
            else_dest,
        } => InstrKind::CondBr {
            cond: f(cond),
            then_dest,
            else_dest,
        },
        InstrKind::Switch {
            val,
            default,
            cases,
        } => InstrKind::Switch {
            val: f(val),
            default,
            cases: cases.into_iter().map(|(v, b)| (f(v), b)).collect(),
        },
        InstrKind::Unreachable => InstrKind::Unreachable,
    }
}
