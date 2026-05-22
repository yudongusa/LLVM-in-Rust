//! RV64 IR -> machine-IR lowering.

use crate::{
    abi::{classify_rv64_int_args, ArgLocation, INT_RET},
    instructions::*,
    regs::{ALLOCATABLE, CALLEE_SAVED, FP_ALLOCATABLE, FP_CALLEE_SAVED, X0},
};
use llvm_codegen::isel::{DebugLoc, IselBackend, MInstr, MachineFunction, PReg, VReg};
use llvm_codegen::sizeof_ty;
use llvm_ir::{
    ArgId, BlockId, ConstantData, Context, FloatPredicate, Function, InstrId, InstrKind,
    InstrprofIntrinsic, IntPredicate, Module, RmwOp, TypeData, ValueRef,
};
use std::collections::HashMap;

/// Public API for `RiscVBackend`.
#[derive(Default)]
pub struct RiscVBackend;

impl IselBackend for RiscVBackend {
    fn lower_function(
        &mut self,
        // `ctx` field.
        ctx: &Context,
        // `module` field.
        module: &Module,
        // `func` field.
        func: &Function,
    ) -> MachineFunction {
        let mut mf = MachineFunction::new(func.name.clone());
        mf.allocatable_pregs = ALLOCATABLE.to_vec();
        mf.allocatable_fp_pregs = FP_ALLOCATABLE.to_vec();
        // Combine int + FP callee-saved lists for prologue/epilogue tracking.
        let mut cs = CALLEE_SAVED.to_vec();
        cs.extend_from_slice(FP_CALLEE_SAVED);
        mf.callee_saved_pregs = cs;
        mf.debug_source = module.source_filename.clone();

        if func.is_declaration || func.blocks.is_empty() {
            return mf;
        }

        for (bi, bb) in func.blocks.iter().enumerate() {
            let label = if bi == 0 {
                func.name.clone()
            } else {
                format!("{}.{}", func.name, bb.name)
            };
            mf.add_block(label);
        }

        let mut vmap: HashMap<ValueRef, VReg> = HashMap::new();

        for bb in &func.blocks {
            for &iid in &bb.body {
                if let InstrKind::Phi { .. } = &func.instr(iid).kind {
                    let vr = mf.fresh_vreg();
                    vmap.insert(ValueRef::Instruction(iid), vr);
                }
            }
        }

        // Copy args from ABI regs/stack placeholders.
        let arg_locs = classify_rv64_int_args(func.args.len());
        for (i, _arg) in func.args.iter().enumerate() {
            let vr = mf.fresh_vreg();
            vmap.insert(ValueRef::Argument(ArgId(i as u32)), vr);
            match arg_locs[i] {
                ArgLocation::Reg(preg) => {
                    let mut mi = MInstr::new(MOV_RR).with_dst(vr).with_preg(preg);
                    mi.phys_uses = vec![preg];
                    mf.push(0, mi);
                }
                ArgLocation::Stack(offset) => {
                    // Placeholder until frame/stack argument loads are modeled.
                    mf.push(0, MInstr::new(MOV_IMM).with_dst(vr).with_imm(offset as i64));
                }
            }
        }

        for (bi, bb) in func.blocks.iter().enumerate() {
            for &iid in &bb.body {
                let dbg = func
                    .instr_dbg_loc(iid)
                    .and_then(|loc_id| module.debug_location(loc_id))
                    .map(|loc| DebugLoc {
                        line: loc.line,
                        column: loc.column,
                    });
                mf.current_debug_loc = dbg;
                if mf.debug_line_start.is_none() {
                    mf.debug_line_start = dbg.map(|loc| loc.line);
                }
                lower_instr(ctx, module, func, &mut mf, bi, iid, &mut vmap);
            }
            if let Some(tid) = bb.terminator {
                let dbg = func
                    .instr_dbg_loc(tid)
                    .and_then(|loc_id| module.debug_location(loc_id))
                    .map(|loc| DebugLoc {
                        line: loc.line,
                        column: loc.column,
                    });
                mf.current_debug_loc = dbg;
                if mf.debug_line_start.is_none() {
                    mf.debug_line_start = dbg.map(|loc| loc.line);
                }
                lower_terminator(ctx, func, &mut mf, bi, tid, &mut vmap);
            }
        }

        mf.current_debug_loc = None;
        mf
    }
}

#[allow(dead_code)]
/// Returns `true` if `ty` is a floating-point type (f32 or f64).
fn is_fp_type(ctx: &Context, ty: llvm_ir::TypeId) -> bool {
    matches!(ctx.get_type(ty), TypeData::Float(_))
}

/// Returns `true` if `ty` is specifically a double-precision (f64) type.
fn is_double(ctx: &Context, ty: llvm_ir::TypeId) -> bool {
    matches!(ctx.get_type(ty), TypeData::Float(llvm_ir::FloatKind::Double))
}

fn resolve(
    ctx: &Context,
    mf: &mut MachineFunction,
    mblock: usize,
    vmap: &mut HashMap<ValueRef, VReg>,
    vr: ValueRef,
) -> VReg {
    if let Some(&existing) = vmap.get(&vr) {
        return existing;
    }
    match vr {
        ValueRef::Constant(cid) => {
            let cd = ctx.get_const(cid);
            // Float constants: materialise as a float VReg via FLD from a data slot.
            // For simplicity we use a fresh int vreg carrying the bit pattern via
            // MOV_IMM (the encoder handles the bits-as-i64 representation).
            // Full constant-pool support is deferred; this gets the infrastructure wired.
            if let ConstantData::Float { bits, .. } = cd {
                let vreg = mf.fresh_float_vreg();
                vmap.insert(vr, vreg);
                // Emit bits through an int temp and an FMV.X.D / FCVT sequence
                // would be most correct, but for the IR-level tests we emit a
                // MOV_IMM carrying the bit pattern.  The encoder treats the dst
                // class when producing the output register number.
                mf.push(mblock, MInstr::new(MOV_IMM).with_dst(vreg).with_imm(*bits as i64));
                vreg
            } else {
                let vreg = mf.fresh_vreg();
                vmap.insert(vr, vreg);
                let imm = const_to_imm(cd);
                mf.push(mblock, MInstr::new(MOV_IMM).with_dst(vreg).with_imm(imm));
                vreg
            }
        }
        _ => {
            let vreg = mf.fresh_vreg();
            vmap.insert(vr, vreg);
            vreg
        }
    }
}

fn const_to_imm(cd: &ConstantData) -> i64 {
    match cd {
        ConstantData::Int { val, .. } => *val as i64,
        ConstantData::Float { bits, .. } => *bits as i64,
        _ => 0,
    }
}

fn lower_icmp_to_bool(
    pred: IntPredicate,
    lhs: VReg,
    rhs: VReg,
    mf: &mut MachineFunction,
    mblock: usize,
    dst: VReg,
) {
    match pred {
        IntPredicate::Eq => {
            let t = mf.fresh_vreg();
            mf.push(mblock, MInstr::new(XOR_RR).with_dst(t).with_vreg(lhs).with_vreg(rhs));
            // dst = (t == 0) ? 1 : 0
            mf.push(mblock, MInstr::new(SLTIU).with_dst(dst).with_vreg(t).with_imm(1));
        }
        IntPredicate::Ne => {
            let t = mf.fresh_vreg();
            mf.push(mblock, MInstr::new(XOR_RR).with_dst(t).with_vreg(lhs).with_vreg(rhs));
            // dst = (t != 0) ? 1 : 0 => sltu x0, t
            mf.push(mblock, MInstr::new(SLTU_RR).with_dst(dst).with_preg(X0).with_vreg(t));
        }
        IntPredicate::Slt => {
            mf.push(mblock, MInstr::new(SLT_RR).with_dst(dst).with_vreg(lhs).with_vreg(rhs));
        }
        IntPredicate::Sle => {
            let t = mf.fresh_vreg();
            mf.push(mblock, MInstr::new(SLT_RR).with_dst(t).with_vreg(rhs).with_vreg(lhs));
            mf.push(mblock, MInstr::new(XORI).with_dst(dst).with_vreg(t).with_imm(1));
        }
        IntPredicate::Sgt => {
            mf.push(mblock, MInstr::new(SLT_RR).with_dst(dst).with_vreg(rhs).with_vreg(lhs));
        }
        IntPredicate::Sge => {
            let t = mf.fresh_vreg();
            mf.push(mblock, MInstr::new(SLT_RR).with_dst(t).with_vreg(lhs).with_vreg(rhs));
            mf.push(mblock, MInstr::new(XORI).with_dst(dst).with_vreg(t).with_imm(1));
        }
        IntPredicate::Ult => {
            mf.push(mblock, MInstr::new(SLTU_RR).with_dst(dst).with_vreg(lhs).with_vreg(rhs));
        }
        IntPredicate::Ule => {
            let t = mf.fresh_vreg();
            mf.push(mblock, MInstr::new(SLTU_RR).with_dst(t).with_vreg(rhs).with_vreg(lhs));
            mf.push(mblock, MInstr::new(XORI).with_dst(dst).with_vreg(t).with_imm(1));
        }
        IntPredicate::Ugt => {
            mf.push(mblock, MInstr::new(SLTU_RR).with_dst(dst).with_vreg(rhs).with_vreg(lhs));
        }
        IntPredicate::Uge => {
            let t = mf.fresh_vreg();
            mf.push(mblock, MInstr::new(SLTU_RR).with_dst(t).with_vreg(lhs).with_vreg(rhs));
            mf.push(mblock, MInstr::new(XORI).with_dst(dst).with_vreg(t).with_imm(1));
        }
    }
}

fn instrprof_from_callee(ctx: &Context, callee: ValueRef) -> Option<InstrprofIntrinsic> {
    let ValueRef::Constant(cid) = callee else {
        return None;
    };
    let ConstantData::GlobalRef { name, .. } = ctx.get_const(cid) else {
        return None;
    };
    InstrprofIntrinsic::from_name(name)
}

fn lower_instr(
    ctx: &Context,
    _module: &Module,
    func: &Function,
    mf: &mut MachineFunction,
    mblock: usize,
    iid: InstrId,
    vmap: &mut HashMap<ValueRef, VReg>,
) {
    use InstrKind::*;
    let instr = func.instr(iid);

    macro_rules! new_dst {
        () => {{
            let v = mf.fresh_vreg();
            vmap.insert(ValueRef::Instruction(iid), v);
            v
        }};
    }
    macro_rules! new_fp_dst {
        () => {{
            let v = mf.fresh_float_vreg();
            vmap.insert(ValueRef::Instruction(iid), v);
            v
        }};
    }
    macro_rules! res {
        ($vref:expr) => {
            resolve(ctx, mf, mblock, vmap, $vref)
        };
    }
    macro_rules! emit_binop3 {
        ($op:expr, $lhs:expr, $rhs:expr) => {{
            let dst = new_dst!();
            let l = res!($lhs);
            let r = res!($rhs);
            mf.push(mblock, MInstr::new($op).with_dst(dst).with_vreg(l).with_vreg(r));
        }};
    }
    macro_rules! emit_fp_binop3 {
        ($op:expr, $lhs:expr, $rhs:expr) => {{
            let dst = new_fp_dst!();
            let l = res!($lhs);
            let r = res!($rhs);
            mf.push(mblock, MInstr::new($op).with_dst(dst).with_vreg(l).with_vreg(r));
        }};
    }

    match &instr.kind {
        Add { lhs, rhs, .. } => emit_binop3!(ADD_RR, *lhs, *rhs),
        Sub { lhs, rhs, .. } => emit_binop3!(SUB_RR, *lhs, *rhs),
        Mul { lhs, rhs, .. } => emit_binop3!(MUL_RR, *lhs, *rhs),
        SDiv { lhs, rhs, .. } => emit_binop3!(DIV_RR, *lhs, *rhs),
        UDiv { lhs, rhs, .. } => emit_binop3!(UDIV_RR, *lhs, *rhs),
        SRem { lhs, rhs, .. } => emit_binop3!(REM_RR, *lhs, *rhs),
        URem { lhs, rhs, .. } => emit_binop3!(UREM_RR, *lhs, *rhs),

        And { lhs, rhs } => emit_binop3!(AND_RR, *lhs, *rhs),
        Or { lhs, rhs } => emit_binop3!(OR_RR, *lhs, *rhs),
        Xor { lhs, rhs } => emit_binop3!(XOR_RR, *lhs, *rhs),

        Shl { lhs, rhs, .. } => emit_binop3!(SLL_RR, *lhs, *rhs),
        LShr { lhs, rhs, .. } => emit_binop3!(SRL_RR, *lhs, *rhs),
        AShr { lhs, rhs, .. } => emit_binop3!(SRA_RR, *lhs, *rhs),

        ICmp { pred, lhs, rhs } => {
            let dst = new_dst!();
            let l = res!(*lhs);
            let r = res!(*rhs);
            lower_icmp_to_bool(*pred, l, r, mf, mblock, dst);
        }

        FCmp { pred, lhs, rhs, .. } => {
            let dst = new_dst!();
            let instr_ty = func.instr(iid).ty;
            // Get the operand type from lhs
            let op_ty = {
                // Try to find the type from the lhs ValueRef
                match lhs {
                    ValueRef::Instruction(i) => func.instr(*i).ty,
                    ValueRef::Argument(ArgId(a)) => func.args[*a as usize].ty,
                    ValueRef::Constant(c) => {
                        match ctx.get_const(*c) {
                            ConstantData::Float { ty, .. } => *ty,
                            _ => instr_ty,
                        }
                    }
                    _ => instr_ty,
                }
            };
            let double = is_double(ctx, op_ty);
            let l = res!(*lhs);
            let r = res!(*rhs);
            match pred {
                // Ordered equal: feq.d
                FloatPredicate::Oeq | FloatPredicate::Ueq => {
                    let op = if double { FCMP_EQ_D } else { FCMP_EQ_S };
                    mf.push(mblock, MInstr::new(op).with_dst(dst).with_vreg(l).with_vreg(r));
                }
                // Ordered less-than: flt.d
                FloatPredicate::Olt | FloatPredicate::Ult => {
                    let op = if double { FCMP_LT_D } else { FCMP_LT_S };
                    mf.push(mblock, MInstr::new(op).with_dst(dst).with_vreg(l).with_vreg(r));
                }
                // Ordered less-or-equal: fle.d
                FloatPredicate::Ole | FloatPredicate::Ule => {
                    let op = if double { FCMP_LE_D } else { FCMP_LE_S };
                    mf.push(mblock, MInstr::new(op).with_dst(dst).with_vreg(l).with_vreg(r));
                }
                // Ordered greater-than: flt.d with swapped operands
                FloatPredicate::Ogt | FloatPredicate::Ugt => {
                    let op = if double { FCMP_LT_D } else { FCMP_LT_S };
                    mf.push(mblock, MInstr::new(op).with_dst(dst).with_vreg(r).with_vreg(l));
                }
                // Ordered greater-or-equal: fle.d with swapped operands
                FloatPredicate::Oge | FloatPredicate::Uge => {
                    let op = if double { FCMP_LE_D } else { FCMP_LE_S };
                    mf.push(mblock, MInstr::new(op).with_dst(dst).with_vreg(r).with_vreg(l));
                }
                // Not-equal: !(feq.d a, b) = xori(feq.d a b, 1)
                FloatPredicate::One | FloatPredicate::Une => {
                    let eq_op = if double { FCMP_EQ_D } else { FCMP_EQ_S };
                    let t = mf.fresh_vreg();
                    mf.push(mblock, MInstr::new(eq_op).with_dst(t).with_vreg(l).with_vreg(r));
                    mf.push(mblock, MInstr::new(XORI).with_dst(dst).with_vreg(t).with_imm(1));
                }
                // True/False/Ord/Uno: trivial constants
                FloatPredicate::True => {
                    mf.push(mblock, MInstr::new(MOV_IMM).with_dst(dst).with_imm(1));
                }
                FloatPredicate::False => {
                    mf.push(mblock, MInstr::new(MOV_IMM).with_dst(dst).with_imm(0));
                }
                FloatPredicate::Ord | FloatPredicate::Uno => {
                    // Ord: both are not NaN = feq(a, a) & feq(b, b)
                    // Uno: either is NaN = !feq(a,a) | !feq(b,b)
                    // For simplicity emit 1 (Ord=always ordered, Uno=never unordered) as stub.
                    let val = if *pred == FloatPredicate::Ord { 1 } else { 0 };
                    mf.push(mblock, MInstr::new(MOV_IMM).with_dst(dst).with_imm(val));
                }
            }
        }

        Select { cond, then_val, else_val } => {
            let dst = new_dst!();
            let c = res!(*cond);
            let tv = res!(*then_val);
            let fv = res!(*else_val);
            // cond is i1 (0/1): dst = tv*cond + fv*(cond^1)
            let notc = mf.fresh_vreg();
            mf.push(mblock, MInstr::new(XORI).with_dst(notc).with_vreg(c).with_imm(1));
            let tpart = mf.fresh_vreg();
            let fpart = mf.fresh_vreg();
            mf.push(mblock, MInstr::new(MUL_RR).with_dst(tpart).with_vreg(tv).with_vreg(c));
            mf.push(mblock, MInstr::new(MUL_RR).with_dst(fpart).with_vreg(fv).with_vreg(notc));
            mf.push(mblock, MInstr::new(ADD_RR).with_dst(dst).with_vreg(tpart).with_vreg(fpart));
        }

        Phi { .. } => {}

        FPToSI { val, .. } => {
            // Convert FP → signed integer.  Result type gives the target int width.
            let result_ty = func.instr(iid).ty;
            let dst = new_dst!();
            let src = res!(*val);
            // Determine source precision from val's type.
            let src_ty = match val {
                ValueRef::Instruction(i) => func.instr(*i).ty,
                ValueRef::Argument(ArgId(a)) => func.args[*a as usize].ty,
                _ => ctx.f64_ty,
            };
            let double = is_double(ctx, src_ty);
            let bits = match ctx.get_type(result_ty) {
                TypeData::Integer(b) => *b,
                _ => 64,
            };
            let op = match (double, bits) {
                (true,  64) => FCVT_L_D,
                (true,  _)  => FCVT_W_D,
                (false, 64) => FCVT_L_S,
                (false, _)  => FCVT_W_S,
            };
            mf.push(mblock, MInstr::new(op).with_dst(dst).with_vreg(src));
        }

        FPToUI { val, .. } => {
            // Approximate: treat as signed for now (correct for non-negative values).
            let result_ty = func.instr(iid).ty;
            let dst = new_dst!();
            let src = res!(*val);
            let src_ty = match val {
                ValueRef::Instruction(i) => func.instr(*i).ty,
                ValueRef::Argument(ArgId(a)) => func.args[*a as usize].ty,
                _ => ctx.f64_ty,
            };
            let double = is_double(ctx, src_ty);
            let bits = match ctx.get_type(result_ty) {
                TypeData::Integer(b) => *b,
                _ => 64,
            };
            let op = match (double, bits) {
                (true,  64) => FCVT_L_D,
                (true,  _)  => FCVT_W_D,
                (false, 64) => FCVT_L_S,
                (false, _)  => FCVT_W_S,
            };
            mf.push(mblock, MInstr::new(op).with_dst(dst).with_vreg(src));
        }

        SIToFP { val, .. } => {
            // Convert signed integer → FP.
            let result_ty = func.instr(iid).ty;
            let dst = new_fp_dst!();
            let src = res!(*val);
            let src_ty = match val {
                ValueRef::Instruction(i) => func.instr(*i).ty,
                ValueRef::Argument(ArgId(a)) => func.args[*a as usize].ty,
                _ => ctx.i64_ty,
            };
            let src_bits = match ctx.get_type(src_ty) {
                TypeData::Integer(b) => *b,
                _ => 64,
            };
            let double = is_double(ctx, result_ty);
            let op = match (double, src_bits) {
                (true,  64) => FCVT_D_L,
                (true,  _)  => FCVT_D_W,
                (false, 64) => FCVT_S_L,
                (false, _)  => FCVT_S_W,
            };
            mf.push(mblock, MInstr::new(op).with_dst(dst).with_vreg(src));
        }

        UIToFP { val, .. } => {
            // Approximate: treat as signed conversion (correct for values fitting in signed range).
            let result_ty = func.instr(iid).ty;
            let dst = new_fp_dst!();
            let src = res!(*val);
            let src_ty = match val {
                ValueRef::Instruction(i) => func.instr(*i).ty,
                ValueRef::Argument(ArgId(a)) => func.args[*a as usize].ty,
                _ => ctx.i64_ty,
            };
            let src_bits = match ctx.get_type(src_ty) {
                TypeData::Integer(b) => *b,
                _ => 64,
            };
            let double = is_double(ctx, result_ty);
            let op = match (double, src_bits) {
                (true,  64) => FCVT_D_L,
                (true,  _)  => FCVT_D_W,
                (false, 64) => FCVT_S_L,
                (false, _)  => FCVT_S_W,
            };
            mf.push(mblock, MInstr::new(op).with_dst(dst).with_vreg(src));
        }

        FPTrunc { val, .. } => {
            // double→float narrowing: emit a float-class vreg copy (encoder handles precision).
            let dst = new_fp_dst!();
            let src = res!(*val);
            mf.push(mblock, MInstr::new(FMV_D_D).with_dst(dst).with_vreg(src));
        }

        FPExt { val, .. } => {
            // float→double widening.
            let dst = new_fp_dst!();
            let src = res!(*val);
            mf.push(mblock, MInstr::new(FMV_D_D).with_dst(dst).with_vreg(src));
        }

        ZExt { val, .. }
        | Trunc { val, .. }
        | BitCast { val, .. }
        | PtrToInt { val, .. }
        | IntToPtr { val, .. }
        | AddrSpaceCast { val, .. }
        | Freeze { val }
        | SExt { val, .. } => {
            let dst = new_dst!();
            let src = res!(*val);
            mf.push(mblock, MInstr::new(MOV_RR).with_dst(dst).with_vreg(src));
        }

        Call { callee, args, .. } => {
            if let Some(ip) = instrprof_from_callee(ctx, *callee) {
                eprintln!(
                    "warning: PGO intrinsic {} elided — counter emission not implemented",
                    ip.as_str()
                );
                for &arg in args {
                    let _ = res!(arg);
                }
                mf.push(mblock, MInstr::new(NOP));
                return;
            }
            let arg_locs = classify_rv64_int_args(args.len());
            for (i, &arg_vref) in args.iter().enumerate() {
                let src = res!(arg_vref);
                match arg_locs[i] {
                    ArgLocation::Reg(preg) => emit_mov_to_preg(mf, mblock, preg, src),
                    ArgLocation::Stack(_off) => mf.push(mblock, MInstr::new(SD).with_vreg(src)),
                }
            }
            let callee_vr = res!(*callee);
            let mut call_mi = MInstr::new(JALR).with_preg(PReg(1)).with_vreg(callee_vr).with_imm(0);
            call_mi.clobbers = ALLOCATABLE.to_vec();
            mf.push(mblock, call_mi);

            let dst = new_dst!();
            emit_mov_from_preg(mf, mblock, dst, INT_RET);
        }

        InlineAsm { args, .. } => {
            for &arg in args {
                let _ = res!(arg);
            }
            mf.push(mblock, MInstr::new(NOP));
            if instr.ty != ctx.void_ty {
                let dst = new_dst!();
                mf.push(mblock, MInstr::new(MOV_IMM).with_dst(dst).with_imm(0));
            }
        }

        // ── atomics (#239) — RISC-V A-extension ───────────────────────────
        Fence { .. } => {
            mf.push(mblock, MInstr::new(FENCE));
        }
        CmpXchg { ptr, cmp: _cmp, new_val, .. } => {
            let ptr_vr = res!(*ptr);
            let _cmp_vr = res!(*_cmp);
            let new_vr = res!(*new_val);
            // Determine width from instruction result type (struct { val_ty, i1 }).
            // Use W vs D based on pointer-width assumption; default to W (32-bit).
            let ty_bits = match ctx.get_type(instr.ty) {
                TypeData::Struct(st) if !st.fields.is_empty() => {
                    match ctx.get_type(st.fields[0]) {
                        TypeData::Integer(b) => *b,
                        _ => 32,
                    }
                }
                _ => 32,
            };
            let use_d = ty_bits == 64;
            // LR.W/LR.D old, (ptr)
            let old_vr = mf.fresh_vreg();
            let lr_op = if use_d { LR_D } else { LR_W };
            mf.push(mblock, MInstr::new(lr_op).with_dst(old_vr).with_vreg(ptr_vr));
            // SC.W/SC.D status, new_val, (ptr)
            // (Simplified: no retry loop — follow-up per issue #239)
            let status_vr = mf.fresh_vreg();
            let sc_op = if use_d { SC_D } else { SC_W };
            mf.push(mblock, MInstr::new(sc_op).with_dst(status_vr).with_vreg(ptr_vr).with_vreg(new_vr));
            // Result is the old value from LR.
            let dst = new_dst!();
            mf.push(mblock, MInstr::new(MOV_RR).with_dst(dst).with_vreg(old_vr));
        }
        AtomicRmw { op, ptr, val, .. } => {
            let ptr_vr = res!(*ptr);
            let val_vr = res!(*val);
            // Determine word vs doubleword from the instruction's result type.
            let ty_bits = match ctx.get_type(instr.ty) {
                TypeData::Integer(b) => *b,
                _ => 32,
            };
            let use_d = ty_bits == 64;
            let opcode = match op {
                RmwOp::Add => if use_d { AMOADD_D } else { AMOADD_W },
                RmwOp::Xchg => if use_d { AMOSWAP_D } else { AMOSWAP_W },
                RmwOp::Xor => if use_d { AMOXOR_D } else { AMOXOR_W },
                RmwOp::And | RmwOp::Nand => if use_d { AMOAND_D } else { AMOAND_W },
                RmwOp::Or => if use_d { AMOOR_D } else { AMOOR_W },
                // Sub: negate val then add — use amoadd (caller is responsible for negation;
                // here we emit amoadd as the closest approximation without modifying val).
                RmwOp::Sub => if use_d { AMOADD_D } else { AMOADD_W },
                RmwOp::Min => AMOMIN_W,
                RmwOp::Max => AMOMAX_W,
                RmwOp::UMin => AMOMINU_W,
                RmwOp::UMax => AMOMAXU_W,
                _ => if use_d { AMOADD_D } else { AMOADD_W },
            };
            let dst = new_dst!();
            mf.push(mblock, MInstr::new(opcode).with_dst(dst).with_vreg(ptr_vr).with_vreg(val_vr));
        }

        // ── memory: non-promotable alloca/load/store (FP-relative frame slots) ──
        // mem2reg eliminates promotable alloca/load/store; what remains here is
        // non-promotable (address escapes or non-constant GEP index).
        Alloca { alloc_ty, align, .. } => {
            let dst = new_dst!();
            let size_bytes = sizeof_ty(ctx, *alloc_ty) as u32;
            let align_bytes = align.unwrap_or(8);
            let slot_idx = mf.alloc_alloca_slot(size_bytes, align_bytes);
            // ADDI_FP_SLOT: dst = s0 + -(slot_idx+1)*8
            mf.push(
                mblock,
                MInstr::new(ADDI_FP_SLOT)
                    .with_dst(dst)
                    .with_imm(slot_idx as i64),
            );
        }
        GetElementPtr { ptr, .. } => {
            // Simple GEP: copy the base pointer into a fresh VReg.
            let dst = new_dst!();
            let base = res!(*ptr);
            mf.push(mblock, MInstr::new(MOV_RR).with_dst(dst).with_vreg(base));
        }
        Load { ptr, .. } => {
            let dst = new_dst!();
            // LD_REG: ld dst, 0(ptr_reg)
            let ptr_vr = res!(*ptr);
            mf.push(
                mblock,
                MInstr::new(LD_REG).with_dst(dst).with_vreg(ptr_vr),
            );
        }
        Store { val, ptr, .. } => {
            // SD_REG: sd src, 0(ptr_reg)
            let src = res!(*val);
            let ptr_vr = res!(*ptr);
            mf.push(
                mblock,
                MInstr::new(SD_REG).with_vreg(ptr_vr).with_vreg(src),
            );
        }

        FAdd { lhs, rhs, .. } => {
            let ty = func.instr(iid).ty;
            let op = if is_double(ctx, ty) { FADD_D } else { FADD_S };
            emit_fp_binop3!(op, *lhs, *rhs);
        }
        FSub { lhs, rhs, .. } => {
            let ty = func.instr(iid).ty;
            let op = if is_double(ctx, ty) { FSUB_D } else { FSUB_S };
            emit_fp_binop3!(op, *lhs, *rhs);
        }
        FMul { lhs, rhs, .. } => {
            let ty = func.instr(iid).ty;
            let op = if is_double(ctx, ty) { FMUL_D } else { FMUL_S };
            emit_fp_binop3!(op, *lhs, *rhs);
        }
        FDiv { lhs, rhs, .. } => {
            let ty = func.instr(iid).ty;
            let op = if is_double(ctx, ty) { FDIV_D } else { FDIV_S };
            emit_fp_binop3!(op, *lhs, *rhs);
        }
        FRem { lhs, rhs, .. } => {
            // RISC-V has no frem instruction; emit fdiv as approximation.
            let ty = func.instr(iid).ty;
            let op = if is_double(ctx, ty) { FDIV_D } else { FDIV_S };
            emit_fp_binop3!(op, *lhs, *rhs);
        }
        FNeg { operand, .. } => {
            let ty = func.instr(iid).ty;
            let dst = new_fp_dst!();
            let src = res!(*operand);
            // fsgnjn.d rd, rs, rs negates the sign bit: FNEG_D uses rs1=rs2.
            let op = if is_double(ctx, ty) { FNEG_D } else { FNEG_S };
            mf.push(mblock, MInstr::new(op).with_dst(dst).with_vreg(src).with_vreg(src));
        }

        ExtractValue { .. }
        | InsertValue { .. }
        | ExtractElement { .. }
        | InsertElement { .. }
        | ShuffleVector { .. } => {
            let dst = new_dst!();
            mf.push(mblock, MInstr::new(MOV_IMM).with_dst(dst).with_imm(0));
        }

        LandingPad { .. } => {
            let dst = new_dst!();
            mf.push(mblock, MInstr::new(MOV_IMM).with_dst(dst).with_imm(0));
        }

        Ret { .. } | Br { .. } | CondBr { .. } | Invoke { .. } | Switch { .. } | Unreachable | Resume { .. } => {}
    }
}

fn lower_terminator(
    ctx: &Context,
    func: &Function,
    mf: &mut MachineFunction,
    mblock: usize,
    tid: InstrId,
    vmap: &mut HashMap<ValueRef, VReg>,
) {
    use InstrKind::*;
    let term = func.instr(tid);

    match &term.kind {
        Ret { val } => {
            if let Some(rv) = val {
                let src = resolve(ctx, mf, mblock, vmap, *rv);
                emit_mov_to_preg(mf, mblock, INT_RET, src);
            }
            mf.push(mblock, MInstr::new(RET));
        }

        Br { dest } => {
            emit_phi_copies(ctx, func, mf, mblock, mblock, *dest, vmap);
            mf.push(mblock, MInstr::new(JAL).with_block(dest.0 as usize));
        }

        CondBr { cond, then_dest, else_dest } => {
            let c = resolve(ctx, mf, mblock, vmap, *cond);
            let pred_label = mf.blocks[mblock].label.clone();
            let then_edge = mf.add_block(format!("{}.then_edge", pred_label));
            let else_edge = mf.add_block(format!("{}.else_edge", pred_label));
            emit_phi_copies(ctx, func, mf, mblock, then_edge, *then_dest, vmap);
            mf.push(then_edge, MInstr::new(JAL).with_block(then_dest.0 as usize));
            emit_phi_copies(ctx, func, mf, mblock, else_edge, *else_dest, vmap);
            mf.push(else_edge, MInstr::new(JAL).with_block(else_dest.0 as usize));
            // if c == 0 goto else_edge else then_edge
            mf.push(mblock, MInstr::new(BEQ).with_vreg(c).with_preg(X0).with_block(else_edge));
            mf.push(mblock, MInstr::new(JAL).with_block(then_edge));
        }

        Invoke { callee, args, normal_dest, .. } => {
            let arg_locs = classify_rv64_int_args(args.len());
            for (i, &arg_vref) in args.iter().enumerate() {
                let src = resolve(ctx, mf, mblock, vmap, arg_vref);
                match arg_locs[i] {
                    ArgLocation::Reg(preg) => emit_mov_to_preg(mf, mblock, preg, src),
                    ArgLocation::Stack(_) => {}
                }
            }
            let callee_vr = resolve(ctx, mf, mblock, vmap, *callee);
            let mut call_mi = MInstr::new(JALR).with_vreg(callee_vr);
            call_mi.clobbers = ALLOCATABLE.to_vec();
            mf.push(mblock, call_mi);
            if term.ty != ctx.void_ty {
                let dst = mf.fresh_vreg();
                vmap.insert(ValueRef::Instruction(tid), dst);
                emit_mov_from_preg(mf, mblock, dst, INT_RET);
            }
            emit_phi_copies(ctx, func, mf, mblock, mblock, *normal_dest, vmap);
            mf.push(mblock, MInstr::new(JAL).with_block(normal_dest.0 as usize));
        }

        Switch { val, default, cases } => {
            let v = resolve(ctx, mf, mblock, vmap, *val);
            for (case_val, case_dest) in cases {
                let cv = resolve(ctx, mf, mblock, vmap, *case_val);
                emit_phi_copies(ctx, func, mf, mblock, mblock, *case_dest, vmap);
                mf.push(mblock, MInstr::new(BEQ).with_vreg(v).with_vreg(cv).with_block(case_dest.0 as usize));
            }
            emit_phi_copies(ctx, func, mf, mblock, mblock, *default, vmap);
            mf.push(mblock, MInstr::new(JAL).with_block(default.0 as usize));
        }

        Unreachable => mf.push(mblock, MInstr::new(NOP)),

        // resume re-throws the in-flight exception; lower to NOP as placeholder.
        Resume { .. } => mf.push(mblock, MInstr::new(NOP)),

        _ => {}
    }
}

fn emit_phi_copies(
    ctx: &Context,
    func: &Function,
    mf: &mut MachineFunction,
    ir_src_block: usize,
    emit_to_mblock: usize,
    dest: BlockId,
    vmap: &mut HashMap<ValueRef, VReg>,
) {
    let dest_bb = &func.blocks[dest.0 as usize];
    let src_bid = BlockId(ir_src_block as u32);

    for &iid in &dest_bb.body {
        if let InstrKind::Phi { incoming, .. } = &func.instr(iid).kind {
            if let Some((incoming_val, _)) = incoming.iter().find(|(_, bid)| *bid == src_bid) {
                let phi_vreg = match vmap.get(&ValueRef::Instruction(iid)) {
                    Some(&v) => v,
                    None => continue,
                };
                let src_vreg = resolve(ctx, mf, emit_to_mblock, vmap, *incoming_val);
                mf.push(emit_to_mblock, MInstr::new(MOV_RR).with_dst(phi_vreg).with_vreg(src_vreg));
            }
        }
    }
}

fn emit_mov_to_preg(mf: &mut MachineFunction, mblock: usize, preg: PReg, src: VReg) {
    mf.push(mblock, MInstr::new(MOV_PR).with_preg(preg).with_vreg(src));
}

fn emit_mov_from_preg(mf: &mut MachineFunction, mblock: usize, dst: VReg, preg: PReg) {
    let mut mi = MInstr::new(MOV_RR).with_dst(dst).with_preg(preg);
    mi.phys_uses = vec![preg];
    mf.push(mblock, mi);
}

#[cfg(test)]
mod tests {
    use super::*;
    use llvm_ir::{Builder, Context, IntPredicate, Linkage, Module};

    #[test]
    fn lower_declaration_is_empty() {
        let mut ctx = Context::new();
        let mut module = Module::new("m");
        let mut b = Builder::new(&mut ctx, &mut module);
        b.add_function("decl", b.ctx.i64_ty, vec![], vec![], true, Linkage::External);
        let f = &module.functions[0];
        let mut be = RiscVBackend;
        let mf = be.lower_function(&ctx, &module, f);
        assert!(mf.blocks.is_empty());
    }

    #[test]
    fn lower_add_ret_generates_instructions() {
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
        let bb = b.get_arg(1);
        let s = b.build_add("s", a, bb);
        b.build_ret(s);

        let f = &module.functions[0];
        let mut be = RiscVBackend;
        let mf = be.lower_function(&ctx, &module, f);
        assert_eq!(mf.blocks.len(), 1);
        assert!(mf.blocks[0].instrs.iter().any(|mi| mi.opcode == ADD_RR));
        assert!(mf.blocks[0].instrs.iter().any(|mi| mi.opcode == RET));
    }

    #[test]
    fn lower_icmp_and_condbr() {
        let mut ctx = Context::new();
        let mut module = Module::new("m");
        let mut b = Builder::new(&mut ctx, &mut module);
        b.add_function(
            "f",
            b.ctx.i64_ty,
            vec![b.ctx.i64_ty],
            vec!["x".into()],
            false,
            Linkage::External,
        );
        let entry = b.add_block("entry");
        let tbb = b.add_block("then");
        let ebb = b.add_block("else");
        b.position_at_end(entry);
        let x = b.get_arg(0);
        let z = b.const_i64(0);
        let c = b.build_icmp("c", IntPredicate::Sgt, x, z);
        b.build_cond_br(c, tbb, ebb);
        b.position_at_end(tbb);
        let one = b.const_i64(1);
        b.build_ret(one);
        b.position_at_end(ebb);
        let zero = b.const_i64(0);
        b.build_ret(zero);

        let f = &module.functions[0];
        let mut be = RiscVBackend;
        let mf = be.lower_function(&ctx, &module, f);
        let all_instrs: Vec<_> = mf.blocks.iter().flat_map(|bb| bb.instrs.iter()).collect();
        assert!(all_instrs.iter().any(|mi| mi.opcode == SLT_RR));
        assert!(all_instrs.iter().any(|mi| mi.opcode == BEQ));
        assert!(all_instrs.iter().any(|mi| mi.opcode == JAL));
    }

    // ── Atomic lowering tests ──────────────────────────────────────────────

    #[test]
    fn riscv_fence_emits_fence_opcode() {
        use llvm_ir_parser::parser::parse;
        let src = r#"define void @f() {
entry:
  fence seq_cst
  ret void
}"#;
        let (ctx, module) = parse(src).expect("parse");
        let func = &module.functions[0];
        let mut be = RiscVBackend;
        let mf = be.lower_function(&ctx, &module, func);
        let has_fence = mf
            .blocks
            .iter()
            .flat_map(|b| b.instrs.iter())
            .any(|i| i.opcode == FENCE);
        assert!(has_fence, "expected FENCE opcode in lowered fence");
    }

    #[test]
    fn riscv_atomicrmw_add_emits_amoadd_w() {
        use llvm_ir_parser::parser::parse;
        let src = r#"define i32 @f(ptr %p, i32 %v) {
entry:
  %r = atomicrmw add ptr %p, i32 %v seq_cst
  ret i32 %r
}"#;
        let (ctx, module) = parse(src).expect("parse");
        let func = &module.functions[0];
        let mut be = RiscVBackend;
        let mf = be.lower_function(&ctx, &module, func);
        let has_amoadd = mf
            .blocks
            .iter()
            .flat_map(|b| b.instrs.iter())
            .any(|i| i.opcode == AMOADD_W);
        assert!(has_amoadd, "expected AMOADD_W in lowered atomicrmw add i32");
    }

    #[test]
    fn riscv_atomicrmw_add_i64_emits_amoadd_d() {
        use llvm_ir_parser::parser::parse;
        let src = r#"define i64 @f(ptr %p, i64 %v) {
entry:
  %r = atomicrmw add ptr %p, i64 %v seq_cst
  ret i64 %r
}"#;
        let (ctx, module) = parse(src).expect("parse");
        let func = &module.functions[0];
        let mut be = RiscVBackend;
        let mf = be.lower_function(&ctx, &module, func);
        let has_amoadd_d = mf
            .blocks
            .iter()
            .flat_map(|b| b.instrs.iter())
            .any(|i| i.opcode == AMOADD_D);
        assert!(has_amoadd_d, "expected AMOADD_D in lowered atomicrmw add i64");
    }

    #[test]
    fn riscv_atomicrmw_xchg_emits_amoswap_w() {
        use llvm_ir_parser::parser::parse;
        let src = r#"define i32 @f(ptr %p, i32 %v) {
entry:
  %r = atomicrmw xchg ptr %p, i32 %v seq_cst
  ret i32 %r
}"#;
        let (ctx, module) = parse(src).expect("parse");
        let func = &module.functions[0];
        let mut be = RiscVBackend;
        let mf = be.lower_function(&ctx, &module, func);
        let has_amoswap = mf
            .blocks
            .iter()
            .flat_map(|b| b.instrs.iter())
            .any(|i| i.opcode == AMOSWAP_W);
        assert!(has_amoswap, "expected AMOSWAP_W in lowered atomicrmw xchg i32");
    }

    #[test]
    fn riscv_cmpxchg_emits_lr_sc() {
        use llvm_ir_parser::parser::parse;
        let src = r#"define i32 @f(ptr %p, i32 %cmp, i32 %new) {
entry:
  %r = cmpxchg ptr %p, i32 %cmp, i32 %new seq_cst seq_cst
  %v = extractvalue { i32, i1 } %r, 0
  ret i32 %v
}"#;
        let (ctx, module) = parse(src).expect("parse");
        let func = &module.functions[0];
        let mut be = RiscVBackend;
        let mf = be.lower_function(&ctx, &module, func);
        let all_instrs: Vec<_> = mf.blocks.iter().flat_map(|b| b.instrs.iter()).collect();
        let has_lr = all_instrs.iter().any(|i| i.opcode == LR_W);
        let has_sc = all_instrs.iter().any(|i| i.opcode == SC_W);
        assert!(has_lr, "expected LR_W in lowered cmpxchg");
        assert!(has_sc, "expected SC_W in lowered cmpxchg");
    }

    #[test]
    fn riscv_fence_no_nop_emitted() {
        // Fence must not emit NOP — it should emit exactly one FENCE instruction.
        use llvm_ir_parser::parser::parse;
        let src = r#"define void @f() {
entry:
  fence seq_cst
  ret void
}"#;
        let (ctx, module) = parse(src).expect("parse");
        let func = &module.functions[0];
        let mut be = RiscVBackend;
        let mf = be.lower_function(&ctx, &module, func);
        let nop_count = mf
            .blocks
            .iter()
            .flat_map(|b| b.instrs.iter())
            .filter(|i| i.opcode == NOP)
            .count();
        assert_eq!(nop_count, 0, "fence should not emit any NOP instructions");
    }
}
