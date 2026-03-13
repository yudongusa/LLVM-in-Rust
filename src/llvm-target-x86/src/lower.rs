//! x86_64 IR → machine-IR lowering.
//!
//! Implements [`IselBackend`] for [`X86Backend`].  Each IR instruction is
//! translated to one or more machine instructions using virtual registers.
//! Phi-destruction (parallel copy insertion) is also handled here.

use crate::{
    abi::{ArgLocation, CallingConvention},
    instructions::*,
    regs::{RCX, RDX, RSP},
};
use llvm_codegen::isel::{DebugLoc, IselBackend, MInstr, MOperand, MachineFunction, PReg, VReg};
use llvm_ir::{
    ArgId, BlockId, ConstantData, Context, FloatKind, Function, InstrId, InstrKind, IntPredicate,
    Module, TypeData, ValueRef,
};
use std::collections::HashMap;

/// CPU feature switches used by x86 lowering decisions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TargetFeatures {
    pub sse42: bool,
    pub avx2: bool,
    pub avx512f: bool,
}

impl TargetFeatures {
    pub const fn baseline() -> Self {
        Self {
            sse42: false,
            avx2: false,
            avx512f: false,
        }
    }

    pub const fn sse42() -> Self {
        Self {
            sse42: true,
            avx2: false,
            avx512f: false,
        }
    }

    pub const fn avx2() -> Self {
        Self {
            sse42: true,
            avx2: true,
            avx512f: false,
        }
    }

    pub const fn avx512f() -> Self {
        Self {
            sse42: true,
            avx2: true,
            avx512f: true,
        }
    }

    pub const fn simd_enabled(self) -> bool {
        self.sse42 || self.avx2 || self.avx512f
    }
}

/// x86_64 instruction-selection backend.
pub struct X86Backend {
    pub features: TargetFeatures,
}

impl Default for X86Backend {
    fn default() -> Self {
        Self {
            features: TargetFeatures::baseline(),
        }
    }
}

impl X86Backend {
    pub const fn new(features: TargetFeatures) -> Self {
        Self { features }
    }
}

impl IselBackend for X86Backend {
    fn lower_function(
        &mut self,
        ctx: &Context,
        module: &Module,
        func: &Function,
    ) -> MachineFunction {
        let cc = CallingConvention::from_target_triple(module.target_triple.as_deref());
        let mut mf = MachineFunction::new(func.name.clone());
        mf.allocatable_pregs = cc.allocatable_pregs().to_vec();
        mf.callee_saved_pregs = cc.callee_saved_pregs().to_vec();
        mf.debug_source = module.source_filename.clone();

        if func.is_declaration || func.blocks.is_empty() {
            return mf;
        }

        // Create one machine block per IR block.
        for (bi, bb) in func.blocks.iter().enumerate() {
            let label = if bi == 0 {
                func.name.clone()
            } else {
                format!("{}.{}", func.name, bb.name)
            };
            mf.add_block(label);
        }

        // VReg map: IR ValueRef → VReg holding its value.
        let mut vmap: HashMap<ValueRef, VReg> = HashMap::new();

        // Pre-allocate VRegs for all phi definitions (phi-destruction).
        for bb in &func.blocks {
            for &iid in &bb.body {
                if let InstrKind::Phi { .. } = &func.instr(iid).kind {
                    let vr = mf.fresh_vreg();
                    vmap.insert(ValueRef::Instruction(iid), vr);
                }
            }
        }

        // Lower function arguments: copy from ABI registers into VRegs.
        let arg_locs = cc.classify_int_args(func.args.len());
        for (i, _arg) in func.args.iter().enumerate() {
            let vr = mf.fresh_vreg();
            vmap.insert(ValueRef::Argument(ArgId(i as u32)), vr);
            match arg_locs[i] {
                ArgLocation::Reg(preg) => {
                    // mov vreg, preg
                    let mut mi = MInstr::new(MOV_RR).with_dst(vr).with_preg(preg);
                    mi.phys_uses = vec![preg];
                    mf.push(0, mi);
                }
                ArgLocation::Stack(offset) => {
                    // Emit a placeholder LEA to mark the stack slot.
                    mf.push(0, MInstr::new(LEA_RI).with_dst(vr).with_imm(offset as i64));
                }
            }
        }

        // Lower each IR block.
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
                lower_instr(
                    ctx,
                    module,
                    func,
                    &mut mf,
                    bi,
                    iid,
                    &mut vmap,
                    cc,
                    self.features,
                );
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
                lower_terminator(ctx, func, &mut mf, bi, tid, &mut vmap, cc);
            }
        }

        mf.current_debug_loc = None;
        mf
    }
}

// ── resolve a ValueRef to a VReg ─────────────────────────────────────────

/// Return the VReg for `vr`, materialising constants as needed.
fn resolve(
    ctx: &Context,
    mf: &mut MachineFunction,
    mblock: usize,
    vmap: &mut HashMap<ValueRef, VReg>,
    vr: ValueRef,
) -> VReg {
    match vr {
        ValueRef::Constant(cid) => {
            // Constants must not be globally memoized across blocks: the first
            // materialization site might not dominate all later uses.
            // Emit a fresh vreg in the current block for each constant use.
            let vreg = mf.fresh_vreg();
            let imm = const_to_imm(ctx.get_const(cid));
            mf.push(mblock, MInstr::new(MOV_RI).with_dst(vreg).with_imm(imm));
            vreg
        }
        _ => {
            if let Some(&existing) = vmap.get(&vr) {
                return existing;
            }
            // Unknown reference — allocate a placeholder VReg.
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

fn pred_to_cc(pred: IntPredicate) -> i64 {
    match pred {
        IntPredicate::Eq => CC_EQ,
        IntPredicate::Ne => CC_NE,
        IntPredicate::Slt => CC_LT,
        IntPredicate::Sle => CC_LE,
        IntPredicate::Sgt => CC_GT,
        IntPredicate::Sge => CC_GE,
        IntPredicate::Ult => CC_ULT,
        IntPredicate::Ule => CC_ULE,
        IntPredicate::Ugt => CC_UGT,
        IntPredicate::Uge => CC_UGE,
    }
}

#[derive(Clone, Copy)]
enum VecIntOp {
    Add,
    Sub,
    Mul,
}

fn vector_int_opcode(
    ctx: &Context,
    ty: llvm_ir::TypeId,
    op: VecIntOp,
    features: TargetFeatures,
) -> Option<llvm_codegen::isel::MOpcode> {
    let TypeData::Vector {
        element,
        len,
        scalable: false,
    } = ctx.get_type(ty)
    else {
        return None;
    };
    let TypeData::Integer(32) = ctx.get_type(*element) else {
        return None;
    };

    let width_supported = match *len {
        4 => features.sse42,
        8 => features.avx2,
        16 => features.avx512f,
        _ => false,
    };
    if !width_supported {
        return None;
    }

    Some(match op {
        VecIntOp::Add => PADDD_RR,
        VecIntOp::Sub => PSUBD_RR,
        VecIntOp::Mul => PMULLD_RR,
    })
}

#[derive(Clone, Copy)]
enum VecFpOp {
    Add,
    Mul,
    Div,
}

fn vector_fp_opcode(
    ctx: &Context,
    ty: llvm_ir::TypeId,
    op: VecFpOp,
    features: TargetFeatures,
) -> Option<llvm_codegen::isel::MOpcode> {
    let TypeData::Vector {
        element,
        len,
        scalable: false,
    } = ctx.get_type(ty)
    else {
        return None;
    };

    match (ctx.get_type(*element), *len, op) {
        // f32 vectors: 128/256/512-bit lanes gate on SSE4.2/AVX2/AVX-512F.
        (TypeData::Float(FloatKind::Single), 4, VecFpOp::Add) if features.sse42 => Some(ADDPS_RR),
        (TypeData::Float(FloatKind::Single), 4, VecFpOp::Mul) if features.sse42 => Some(MULPS_RR),
        (TypeData::Float(FloatKind::Single), 4, VecFpOp::Div) if features.sse42 => Some(DIVPS_RR),
        (TypeData::Float(FloatKind::Single), 8, VecFpOp::Add) if features.avx2 => Some(ADDPS_RR),
        (TypeData::Float(FloatKind::Single), 8, VecFpOp::Mul) if features.avx2 => Some(MULPS_RR),
        (TypeData::Float(FloatKind::Single), 8, VecFpOp::Div) if features.avx2 => Some(DIVPS_RR),
        (TypeData::Float(FloatKind::Single), 16, VecFpOp::Add) if features.avx512f => Some(ADDPS_RR),
        (TypeData::Float(FloatKind::Single), 16, VecFpOp::Mul) if features.avx512f => Some(MULPS_RR),
        (TypeData::Float(FloatKind::Single), 16, VecFpOp::Div) if features.avx512f => Some(DIVPS_RR),

        // f64 vectors: 128/256/512-bit lanes gate on SSE4.2/AVX2/AVX-512F.
        (TypeData::Float(FloatKind::Double), 2, VecFpOp::Add) if features.sse42 => Some(ADDPD_RR),
        (TypeData::Float(FloatKind::Double), 2, VecFpOp::Mul) if features.sse42 => Some(MULPD_RR),
        (TypeData::Float(FloatKind::Double), 4, VecFpOp::Add) if features.avx2 => Some(ADDPD_RR),
        (TypeData::Float(FloatKind::Double), 4, VecFpOp::Mul) if features.avx2 => Some(MULPD_RR),
        (TypeData::Float(FloatKind::Double), 8, VecFpOp::Add) if features.avx512f => Some(ADDPD_RR),
        (TypeData::Float(FloatKind::Double), 8, VecFpOp::Mul) if features.avx512f => Some(MULPD_RR),
        _ => None,
    }
}

fn const_u64(ctx: &Context, v: ValueRef) -> Option<u64> {
    let ValueRef::Constant(cid) = v else {
        return None;
    };
    match ctx.get_const(cid) {
        ConstantData::Int { val, .. } => Some(*val),
        _ => None,
    }
}

fn const_i64(ctx: &Context, v: ValueRef) -> Option<i64> {
    let ValueRef::Constant(cid) = v else {
        return None;
    };
    match ctx.get_const(cid) {
        ConstantData::Int { val, .. } => i64::try_from(*val).ok(),
        _ => None,
    }
}

// ── instruction lowering ──────────────────────────────────────────────────

fn lower_instr(
    ctx: &Context,
    _module: &Module,
    func: &Function,
    mf: &mut MachineFunction,
    mblock: usize,
    iid: InstrId,
    vmap: &mut HashMap<ValueRef, VReg>,
    cc: CallingConvention,
    features: TargetFeatures,
) {
    use InstrKind::*;
    let instr = func.instr(iid);

    // Helper: allocate a fresh dst VReg and register it.
    macro_rules! new_dst {
        () => {{
            let v = mf.fresh_vreg();
            vmap.insert(ValueRef::Instruction(iid), v);
            v
        }};
    }
    // Helper: resolve a ValueRef.
    macro_rules! res {
        ($vref:expr) => {
            resolve(ctx, mf, mblock, vmap, $vref)
        };
    }
    // Helper: emit a two-input binary op as: dst=mov(lhs); op(dst,rhs).
    // The binary op instruction carries only the RHS in operands (not dst
    // itself) so that `get_dst_src` in the encoder sees the correct source.
    macro_rules! emit_binop {
        ($op:expr, $lhs:expr, $rhs:expr) => {{
            let dst = new_dst!();
            let l = res!($lhs);
            let r = res!($rhs);
            mf.push(mblock, MInstr::new(MOV_RR).with_dst(dst).with_vreg(l));
            mf.push(mblock, MInstr::new($op).with_dst(dst).with_vreg(r));
        }};
    }
    // Helper: emit a variable shift — loads the count into RCX (CL) first,
    // then emits the shift instruction with phys_uses=[RCX].
    // x86 variable shifts require the count in CL (low byte of RCX).
    macro_rules! emit_shift {
        ($op:expr, $lhs:expr, $rhs:expr) => {{
            let dst = new_dst!();
            let l = res!($lhs);
            let r = res!($rhs);
            mf.push(mblock, MInstr::new(MOV_RR).with_dst(dst).with_vreg(l));
            emit_mov_to_preg(mf, mblock, RCX, r);
            let mut shift_mi = MInstr::new($op).with_dst(dst);
            shift_mi.phys_uses = vec![RCX];
            shift_mi.clobbers = vec![RCX];
            mf.push(mblock, shift_mi);
        }};
    }

    match &instr.kind {
        // ── arithmetic ─────────────────────────────────────────────────────
        Add { lhs, rhs, .. } => {
            if let Some(vop) = vector_int_opcode(ctx, instr.ty, VecIntOp::Add, features) {
                emit_binop!(vop, *lhs, *rhs);
            } else {
                emit_binop!(ADD_RR, *lhs, *rhs);
            }
        }
        Sub { lhs, rhs, .. } => {
            if let Some(vop) = vector_int_opcode(ctx, instr.ty, VecIntOp::Sub, features) {
                emit_binop!(vop, *lhs, *rhs);
            } else {
                emit_binop!(SUB_RR, *lhs, *rhs);
            }
        }
        Mul { lhs, rhs, .. } => {
            if let Some(vop) = vector_int_opcode(ctx, instr.ty, VecIntOp::Mul, features) {
                emit_binop!(vop, *lhs, *rhs);
            } else if let Some(k) = const_i64(ctx, *rhs).filter(|v| i32::try_from(*v).is_ok()) {
                // Pattern combine (isel bridge): `%x * C` -> `imul dst, dst, imm32`.
                // This avoids materializing the constant in a separate register.
                let dst = new_dst!();
                let l = res!(*lhs);
                mf.push(mblock, MInstr::new(MOV_RR).with_dst(dst).with_vreg(l));
                mf.push(mblock, MInstr::new(IMUL_RRI).with_dst(dst).with_vreg(dst).with_imm(k));
            } else if let Some(k) = const_i64(ctx, *lhs).filter(|v| i32::try_from(*v).is_ok()) {
                let dst = new_dst!();
                let r = res!(*rhs);
                mf.push(mblock, MInstr::new(MOV_RR).with_dst(dst).with_vreg(r));
                mf.push(mblock, MInstr::new(IMUL_RRI).with_dst(dst).with_vreg(dst).with_imm(k));
            } else {
                emit_binop!(IMUL_RR, *lhs, *rhs);
            }
        }

        SDiv { lhs, rhs, .. } => {
            let dst = new_dst!();
            let l = res!(*lhs);
            let r = res!(*rhs);
            // mov rax, lhs; cqo; idiv rcx → rax = quotient (signed)
            // Keep divisor out of clobbered regs (rax/rdx) to avoid
            // self-clobbering when the allocator picks those registers.
            emit_mov_to_preg(mf, mblock, RCX, r);
            emit_mov_to_preg(mf, mblock, cc.int_ret(), l);
            mf.push(mblock, MInstr::new(CQO));
            let mut div_mi = MInstr::new(IDIV_R).with_preg(RCX);
            div_mi.phys_uses = vec![RCX];
            div_mi.clobbers = vec![cc.int_ret(), RDX];
            mf.push(mblock, div_mi);
            emit_mov_from_preg(mf, mblock, dst, cc.int_ret());
        }

        UDiv { lhs, rhs, .. } => {
            let dst = new_dst!();
            let l = res!(*lhs);
            let r = res!(*rhs);
            // mov rax, lhs; xor rdx, rdx; div rcx → rax = quotient (unsigned)
            emit_mov_to_preg(mf, mblock, RCX, r);
            emit_mov_to_preg(mf, mblock, cc.int_ret(), l);
            let zero = mf.fresh_vreg();
            mf.push(mblock, MInstr::new(MOV_RI).with_dst(zero).with_imm(0));
            emit_mov_to_preg(mf, mblock, RDX, zero);
            let mut div_mi = MInstr::new(DIV_R).with_preg(RCX);
            div_mi.phys_uses = vec![RCX];
            div_mi.clobbers = vec![cc.int_ret(), RDX];
            mf.push(mblock, div_mi);
            emit_mov_from_preg(mf, mblock, dst, cc.int_ret());
        }

        SRem { lhs, rhs, .. } => {
            let dst = new_dst!();
            let l = res!(*lhs);
            let r = res!(*rhs);
            // mov rax, lhs; cqo; idiv rcx → rdx = remainder (signed)
            emit_mov_to_preg(mf, mblock, RCX, r);
            emit_mov_to_preg(mf, mblock, cc.int_ret(), l);
            mf.push(mblock, MInstr::new(CQO));
            let mut div_mi = MInstr::new(IDIV_R).with_preg(RCX);
            div_mi.phys_uses = vec![RCX];
            div_mi.clobbers = vec![cc.int_ret(), RDX];
            mf.push(mblock, div_mi);
            emit_mov_from_preg(mf, mblock, dst, RDX);
        }

        URem { lhs, rhs, .. } => {
            let dst = new_dst!();
            let l = res!(*lhs);
            let r = res!(*rhs);
            // mov rax, lhs; xor rdx, rdx; div rcx → rdx = remainder (unsigned)
            emit_mov_to_preg(mf, mblock, RCX, r);
            emit_mov_to_preg(mf, mblock, cc.int_ret(), l);
            let zero = mf.fresh_vreg();
            mf.push(mblock, MInstr::new(MOV_RI).with_dst(zero).with_imm(0));
            emit_mov_to_preg(mf, mblock, RDX, zero);
            let mut div_mi = MInstr::new(DIV_R).with_preg(RCX);
            div_mi.phys_uses = vec![RCX];
            div_mi.clobbers = vec![cc.int_ret(), RDX];
            mf.push(mblock, div_mi);
            emit_mov_from_preg(mf, mblock, dst, RDX);
        }

        // ── bitwise ────────────────────────────────────────────────────────
        And { lhs, rhs } => {
            emit_binop!(AND_RR, *lhs, *rhs);
        }
        Or { lhs, rhs } => {
            emit_binop!(OR_RR, *lhs, *rhs);
        }
        Xor { lhs, rhs } => {
            emit_binop!(XOR_RR, *lhs, *rhs);
        }

        // ── shifts ─────────────────────────────────────────────────────────
        // x86 variable shifts require the count in CL (low byte of RCX).
        // emit_shift! (defined above) loads rhs into RCX then emits the shift.
        Shl { lhs, rhs, .. } => {
            emit_shift!(SHL_RR, *lhs, *rhs);
        }
        LShr { lhs, rhs, .. } => {
            emit_shift!(SHR_RR, *lhs, *rhs);
        }
        AShr { lhs, rhs, .. } => {
            emit_shift!(SAR_RR, *lhs, *rhs);
        }

        // ── comparisons ────────────────────────────────────────────────────
        ICmp { pred, lhs, rhs } => {
            let dst = new_dst!();
            let l = res!(*lhs);
            let r = res!(*rhs);
            let cc = pred_to_cc(*pred);
            mf.push(mblock, MInstr::new(CMP_RR).with_vreg(l).with_vreg(r));
            mf.push(mblock, MInstr::new(SETCC).with_dst(dst).with_imm(cc));
        }

        FCmp { .. } => {
            // FP comparisons not yet supported — emit a zero.
            let dst = new_dst!();
            mf.push(mblock, MInstr::new(MOV_RI).with_dst(dst).with_imm(0));
        }

        // ── select ─────────────────────────────────────────────────────────
        Select {
            cond,
            then_val,
            else_val,
        } => {
            let dst = new_dst!();
            let c = res!(*cond);
            let tv = res!(*then_val);
            let fv = res!(*else_val);
            mf.push(mblock, MInstr::new(TEST_RR).with_vreg(c).with_vreg(c));
            // Build an all-ones/zero mask from cond, then select via bit ops:
            //   mask = (cond != 0) ? -1 : 0
            //   dst = (then & mask) | (else & ~mask)
            let scratch = mf.fresh_vreg();
            mf.push(mblock, MInstr::new(SETCC).with_dst(scratch).with_imm(CC_NE));
            mf.push(
                mblock,
                MInstr::new(NEG_R).with_dst(scratch).with_vreg(scratch),
            );
            let then_masked = mf.fresh_vreg();
            let else_masked = mf.fresh_vreg();
            mf.push(
                mblock,
                MInstr::new(MOV_RR).with_dst(then_masked).with_vreg(tv),
            );
            mf.push(
                mblock,
                MInstr::new(AND_RR).with_dst(then_masked).with_vreg(scratch),
            );
            mf.push(
                mblock,
                MInstr::new(NOT_R).with_dst(scratch).with_vreg(scratch),
            );
            mf.push(
                mblock,
                MInstr::new(MOV_RR).with_dst(else_masked).with_vreg(fv),
            );
            mf.push(
                mblock,
                MInstr::new(AND_RR).with_dst(else_masked).with_vreg(scratch),
            );
            mf.push(
                mblock,
                MInstr::new(MOV_RR).with_dst(dst).with_vreg(then_masked),
            );
            mf.push(
                mblock,
                MInstr::new(OR_RR).with_dst(dst).with_vreg(else_masked),
            );
        }

        // ── phi ────────────────────────────────────────────────────────────
        Phi { .. } => {
            // VReg was pre-allocated; copies are inserted by phi-destruction
            // in lower_terminator.  Nothing to do here.
        }

        // ── casts ──────────────────────────────────────────────────────────
        ZExt { val, .. }
        | Trunc { val, .. }
        | BitCast { val, .. }
        | PtrToInt { val, .. }
        | IntToPtr { val, .. }
        | FPTrunc { val, .. }
        | FPExt { val, .. }
        | FPToUI { val, .. }
        | FPToSI { val, .. }
        | UIToFP { val, .. }
        | SIToFP { val, .. }
        | AddrSpaceCast { val, .. } => {
            let dst = new_dst!();
            let src = res!(*val);
            mf.push(mblock, MInstr::new(MOV_RR).with_dst(dst).with_vreg(src));
        }

        SExt { val, .. } => {
            let dst = new_dst!();
            let src = res!(*val);
            // Select the correct sign-extension opcode based on source bit width.
            let src_bits = func
                .type_of_value(*val)
                .map(|tid| match ctx.get_type(tid) {
                    TypeData::Integer(bits) => *bits,
                    _ => 32,
                })
                .unwrap_or(32);
            let opcode = if src_bits <= 8 {
                MOVSX_8
            } else if src_bits <= 16 {
                MOVSX_16
            } else {
                MOVSX_32
            };
            mf.push(mblock, MInstr::new(opcode).with_dst(dst).with_vreg(src));
        }

        // ── calls ──────────────────────────────────────────────────────────
        Call { callee, args, .. } => {
            let callee_src = res!(*callee);
            let callee_vr = mf.fresh_vreg();
            mf.push(
                mblock,
                MInstr::new(MOV_RR)
                    .with_dst(callee_vr)
                    .with_vreg(callee_src),
            );

            let arg_locs = cc.classify_int_args(args.len());
            let mut reg_moves: Vec<(PReg, VReg)> = Vec::new();
            let mut stack_args: Vec<VReg> = Vec::new();
            for (i, &arg_vref) in args.iter().enumerate() {
                let src = res!(arg_vref);
                match arg_locs[i] {
                    ArgLocation::Reg(preg) => reg_moves.push((preg, src)),
                    ArgLocation::Stack(_) => stack_args.push(src),
                }
            }

            // Maintain 16-byte stack alignment at the call site.
            let align_pad = if stack_args.len() % 2 == 1 { 8 } else { 0 };
            if align_pad != 0 {
                emit_stack_adjust(mf, mblock, -(align_pad as i64));
            }

            // Stack arguments are pushed right-to-left.
            for src in stack_args.iter().rev() {
                mf.push(mblock, MInstr::new(PUSH_R).with_vreg(*src));
            }

            let shadow = cc.shadow_space_bytes();
            if shadow != 0 {
                emit_stack_adjust(mf, mblock, -(shadow as i64));
            }

            // Two-phase register assignment avoids clobber cycles.
            let mut staged = Vec::with_capacity(reg_moves.len());
            for (_, src) in &reg_moves {
                let tmp = mf.fresh_vreg();
                mf.push(mblock, MInstr::new(MOV_RR).with_dst(tmp).with_vreg(*src));
                staged.push(tmp);
            }
            for ((preg, _), tmp) in reg_moves.iter().zip(staged.into_iter()) {
                emit_mov_to_preg(mf, mblock, *preg, tmp);
            }

            let mut call_mi = MInstr::new(CALL_R).with_vreg(callee_vr);
            call_mi.clobbers = cc.caller_saved_clobbers().to_vec();
            mf.push(mblock, call_mi);

            let cleanup = shadow as i64 + (stack_args.len() as i64) * 8 + align_pad as i64;
            if cleanup != 0 {
                emit_stack_adjust(mf, mblock, cleanup);
            }

            // Capture return value from RAX.
            let dst = new_dst!();
            emit_mov_from_preg(mf, mblock, dst, cc.int_ret());
        }

        // ── memory (placeholder NOP — mem2reg removes most alloca/load/store) ──
        Alloca { .. } | GetElementPtr { .. } => {
            let dst = new_dst!();
            mf.push(mblock, MInstr::new(NOP));
            let _ = dst;
        }
        Load { ty, .. } => {
            let dst = new_dst!();
            if matches!(ctx.get_type(*ty), TypeData::Vector { .. }) && features.simd_enabled() {
                mf.push(
                    mblock,
                    MInstr::new(MOVDQU_LOAD_MR).with_dst(dst).with_imm(0),
                );
            } else {
                mf.push(mblock, MInstr::new(NOP));
            }
        }
        Store { val, .. } => {
            if let Some(ty) = func.type_of_value(*val) {
                if matches!(ctx.get_type(ty), TypeData::Vector { .. }) && features.simd_enabled() {
                    let src = res!(*val);
                    mf.push(
                        mblock,
                        MInstr::new(MOVDQU_STORE_RM).with_imm(0).with_vreg(src),
                    );
                    return;
                }
            }
            mf.push(mblock, MInstr::new(NOP));
        }

        // ── FP arithmetic (not yet supported) ──────────────────────────────
        FAdd { lhs, rhs, .. } => {
            if let Some(vop) = vector_fp_opcode(ctx, instr.ty, VecFpOp::Add, features) {
                emit_binop!(vop, *lhs, *rhs);
            } else {
                let dst = new_dst!();
                mf.push(mblock, MInstr::new(MOV_RI).with_dst(dst).with_imm(0));
            }
        }
        FMul { lhs, rhs, .. } => {
            if let Some(vop) = vector_fp_opcode(ctx, instr.ty, VecFpOp::Mul, features) {
                emit_binop!(vop, *lhs, *rhs);
            } else {
                let dst = new_dst!();
                mf.push(mblock, MInstr::new(MOV_RI).with_dst(dst).with_imm(0));
            }
        }
        FDiv { lhs, rhs, .. } => {
            if let Some(vop) = vector_fp_opcode(ctx, instr.ty, VecFpOp::Div, features) {
                emit_binop!(vop, *lhs, *rhs);
            } else {
                let dst = new_dst!();
                mf.push(mblock, MInstr::new(MOV_RI).with_dst(dst).with_imm(0));
            }
        }
        FSub { .. } | FRem { .. } | FNeg { .. } => {
            let dst = new_dst!();
            mf.push(mblock, MInstr::new(MOV_RI).with_dst(dst).with_imm(0));
        }

        // ── aggregate / vector ops (not yet supported) ─────────────────────
        ExtractValue { .. } | InsertValue { .. } | ShuffleVector { .. } => {
            let dst = new_dst!();
            if features.simd_enabled() {
                // Feature-aware placeholder: SIMD-specific lowering lands in
                // follow-up issue #86 patches, but the path is now gated.
                mf.push(mblock, MInstr::new(MOV_RI).with_dst(dst).with_imm(0));
            } else {
                // Baseline scalar fallback for unsupported vector code paths.
                mf.push(mblock, MInstr::new(MOV_RI).with_dst(dst).with_imm(0));
            }
        }
        ExtractElement { vec, idx } => {
            let dst = new_dst!();
            if features.simd_enabled() && const_u64(ctx, *idx) == Some(0) {
                let src = res!(*vec);
                mf.push(mblock, MInstr::new(MOV_RR).with_dst(dst).with_vreg(src));
            } else {
                mf.push(mblock, MInstr::new(MOV_RI).with_dst(dst).with_imm(0));
            }
        }
        InsertElement { vec, val, idx } => {
            let dst = new_dst!();
            if features.simd_enabled() && const_u64(ctx, *idx) == Some(0) {
                let src = res!(*val);
                mf.push(mblock, MInstr::new(MOV_RR).with_dst(dst).with_vreg(src));
            } else {
                let src = res!(*vec);
                if features.simd_enabled() {
                    mf.push(mblock, MInstr::new(MOVAPS_RR).with_dst(dst).with_vreg(src));
                } else {
                    mf.push(mblock, MInstr::new(MOV_RI).with_dst(dst).with_imm(0));
                }
            }
        }

        // Terminators handled in lower_terminator.
        Ret { .. } | Br { .. } | CondBr { .. } | Switch { .. } | Unreachable => {}
    }
}

// ── terminator lowering ───────────────────────────────────────────────────

fn lower_terminator(
    ctx: &Context,
    func: &Function,
    mf: &mut MachineFunction,
    mblock: usize,
    tid: InstrId,
    vmap: &mut HashMap<ValueRef, VReg>,
    cc: CallingConvention,
) {
    use InstrKind::*;
    let term = func.instr(tid);

    match &term.kind {
        Ret { val } => {
            if let Some(rv) = val {
                let src = resolve(ctx, mf, mblock, vmap, *rv);
                emit_mov_to_preg(mf, mblock, cc.int_ret(), src);
            }
            mf.push(mblock, MInstr::new(RET));
        }

        Br { dest } => {
            emit_phi_copies(ctx, func, mf, mblock, mblock, *dest, vmap);
            mf.push(mblock, MInstr::new(JMP).with_block(dest.0 as usize));
        }

        CondBr {
            cond,
            then_dest,
            else_dest,
        } => {
            let c = resolve(ctx, mf, mblock, vmap, *cond);
            // Each successor edge gets its own trampoline block so that phi
            // copies for one edge cannot overwrite values needed by the other.
            let pred_label = mf.blocks[mblock].label.clone();
            let then_edge = mf.add_block(format!("{}.then_edge", pred_label));
            let else_edge = mf.add_block(format!("{}.else_edge", pred_label));
            emit_phi_copies(ctx, func, mf, mblock, then_edge, *then_dest, vmap);
            mf.push(then_edge, MInstr::new(JMP).with_block(then_dest.0 as usize));
            emit_phi_copies(ctx, func, mf, mblock, else_edge, *else_dest, vmap);
            mf.push(else_edge, MInstr::new(JMP).with_block(else_dest.0 as usize));
            mf.push(mblock, MInstr::new(TEST_RR).with_vreg(c).with_vreg(c));
            mf.push(
                mblock,
                MInstr::new(JCC).with_imm(CC_NE).with_block(then_edge),
            );
            mf.push(mblock, MInstr::new(JMP).with_block(else_edge));
        }

        Switch {
            val,
            default,
            cases,
        } => {
            let v = resolve(ctx, mf, mblock, vmap, *val);
            for (case_val, case_dest) in cases {
                let cv = resolve(ctx, mf, mblock, vmap, *case_val);
                emit_phi_copies(ctx, func, mf, mblock, mblock, *case_dest, vmap);
                mf.push(mblock, MInstr::new(CMP_RR).with_vreg(v).with_vreg(cv));
                mf.push(
                    mblock,
                    MInstr::new(JCC)
                        .with_imm(CC_EQ)
                        .with_block(case_dest.0 as usize),
                );
            }
            emit_phi_copies(ctx, func, mf, mblock, mblock, *default, vmap);
            mf.push(mblock, MInstr::new(JMP).with_block(default.0 as usize));
        }

        Unreachable => {
            mf.push(mblock, MInstr::new(NOP));
        }

        _ => {} // body instructions already handled
    }
}

// ── phi destruction ───────────────────────────────────────────────────────

/// For each phi in `dest`, emit a copy from the incoming value into the
/// phi's pre-allocated VReg.
///
/// * `ir_src_block` — IR `BlockId` index of the predecessor block, used to
///   match `phi.incoming` entries.
/// * `emit_to_mblock` — machine block where the copy instructions are placed;
///   for a `CondBr` this is a trampoline block, not the predecessor itself.
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
    let mut copies: Vec<(VReg, VReg)> = Vec::new();
    for &iid in &dest_bb.body {
        if let InstrKind::Phi { incoming, .. } = &func.instr(iid).kind {
            if let Some((incoming_val, _)) = incoming.iter().find(|(_, bid)| *bid == src_bid) {
                let phi_vreg = match vmap.get(&ValueRef::Instruction(iid)) {
                    Some(&v) => v,
                    None => continue,
                };
                let src_vreg = resolve(ctx, mf, emit_to_mblock, vmap, *incoming_val);
                if phi_vreg != src_vreg {
                    copies.push((phi_vreg, src_vreg));
                }
            }
        }
    }

    // Correct parallel-copy lowering: first snapshot all sources into temps,
    // then assign destinations from those temps. This avoids all cycle/clobber
    // hazards at the cost of extra moves.
    let mut staged: Vec<(VReg, VReg)> = Vec::with_capacity(copies.len());
    for (dst, src) in copies {
        let tmp = mf.fresh_vreg();
        mf.push(
            emit_to_mblock,
            MInstr::new(MOV_RR).with_dst(tmp).with_vreg(src),
        );
        staged.push((dst, tmp));
    }
    for (dst, tmp) in staged {
        mf.push(
            emit_to_mblock,
            MInstr::new(MOV_RR).with_dst(dst).with_vreg(tmp),
        );
    }
}

// ── ABI register helpers ──────────────────────────────────────────────────

fn emit_mov_to_preg(mf: &mut MachineFunction, mblock: usize, preg: PReg, src: VReg) {
    // MOV_PR: operands[0] = fixed PReg destination, operands[1] = VReg source.
    // dst is intentionally None so apply_allocation does not reassign preg.
    // After regalloc operands[1] becomes PReg(src_allocated) and the encoder
    // generates `mov preg, src_allocated`.
    mf.push(mblock, MInstr::new(MOV_PR).with_preg(preg).with_vreg(src));
}

fn emit_stack_adjust(mf: &mut MachineFunction, mblock: usize, delta: i64) {
    if delta == 0 {
        return;
    }
    let mut mi = if delta > 0 {
        MInstr::new(ADD_RI)
    } else {
        MInstr::new(SUB_RI)
    };
    mi.operands.push(MOperand::PReg(RSP));
    mi.operands.push(MOperand::Imm(delta.unsigned_abs() as i64));
    mi.phys_uses = vec![RSP];
    mi.clobbers = vec![RSP];
    mf.push(mblock, mi);
}

fn emit_mov_from_preg(mf: &mut MachineFunction, mblock: usize, dst: VReg, preg: PReg) {
    let mut mi = MInstr::new(MOV_RR).with_dst(dst).with_preg(preg);
    mi.phys_uses = vec![preg];
    mf.push(mblock, mi);
}

// ── tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use llvm_codegen::isel::MOperand;
    use llvm_ir::{Builder, Context, Linkage, Module};

    fn make_add_fn() -> (Context, Module) {
        let mut ctx = Context::new();
        let mut module = Module::new("test");
        let mut b = Builder::new(&mut ctx, &mut module);
        b.add_function(
            "add",
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
        let sum = b.build_add("sum", a, bv);
        b.build_ret(sum);
        (ctx, module)
    }

    #[test]
    fn lower_add_produces_machine_blocks() {
        let (ctx, module) = make_add_fn();
        let mut be = X86Backend::default();
        let mf = be.lower_function(&ctx, &module, &module.functions[0]);
        assert_eq!(mf.name, "add");
        assert!(!mf.blocks.is_empty());
    }

    #[test]
    fn lower_add_has_ret_instruction() {
        let (ctx, module) = make_add_fn();
        let mut be = X86Backend::default();
        let mf = be.lower_function(&ctx, &module, &module.functions[0]);
        let has_ret = mf
            .blocks
            .iter()
            .any(|b| b.instrs.iter().any(|i| i.opcode == RET));
        assert!(has_ret, "machine function must contain a RET");
    }

    #[test]
    fn lower_add_allocatable_set() {
        let (ctx, module) = make_add_fn();
        let mut be = X86Backend::default();
        let mf = be.lower_function(&ctx, &module, &module.functions[0]);
        assert!(!mf.allocatable_pregs.is_empty());
    }

    #[test]
    fn entry_arg_registers_follow_sysv_vs_win64() {
        let (ctx, mut module) = make_add_fn();
        let mut be = X86Backend::default();

        // SysV default: %a in RDI, %b in RSI.
        let mf_sysv = be.lower_function(&ctx, &module, &module.functions[0]);
        let uses_sysv: Vec<_> = mf_sysv.blocks[0]
            .instrs
            .iter()
            .filter(|i| i.opcode == MOV_RR)
            .filter_map(|i| i.phys_uses.first().copied())
            .collect();
        assert!(uses_sysv.starts_with(&[crate::regs::RDI, crate::regs::RSI]));

        // Win64 triple: %a in RCX, %b in RDX.
        module.target_triple = Some("x86_64-pc-windows-msvc".into());
        let mf_win = be.lower_function(&ctx, &module, &module.functions[0]);
        let uses_win: Vec<_> = mf_win.blocks[0]
            .instrs
            .iter()
            .filter(|i| i.opcode == MOV_RR)
            .filter_map(|i| i.phys_uses.first().copied())
            .collect();
        assert!(uses_win.starts_with(&[crate::regs::RCX, crate::regs::RDX]));
    }

    #[test]
    fn win64_register_sets_mark_rsi_rdi_callee_saved() {
        let (ctx, mut module) = make_add_fn();
        module.target_triple = Some("x86_64-pc-windows-msvc".into());
        let mut be = X86Backend::default();
        let mf = be.lower_function(&ctx, &module, &module.functions[0]);
        assert!(mf.callee_saved_pregs.contains(&crate::regs::RSI));
        assert!(mf.callee_saved_pregs.contains(&crate::regs::RDI));
        assert!(!mf.allocatable_pregs.contains(&crate::regs::RSI));
        assert!(!mf.allocatable_pregs.contains(&crate::regs::RDI));
    }

    fn make_call_fn(n_args: usize) -> (Context, Module) {
        let mut ctx = Context::new();
        let mut module = Module::new("call");
        let mut b = Builder::new(&mut ctx, &mut module);
        let i64_ty = b.ctx.i64_ty;
        let arg_tys = vec![i64_ty; n_args];
        let callee_ty = b.ctx.mk_fn_type(i64_ty, arg_tys.clone(), false);
        b.add_declaration("callee", i64_ty, arg_tys.clone(), false);
        b.add_function("caller", i64_ty, vec![], vec![], false, Linkage::External);
        let entry = b.add_block("entry");
        b.position_at_end(entry);
        let mut args = Vec::with_capacity(n_args);
        for i in 0..n_args {
            args.push(ValueRef::Constant(b.ctx.const_int(i64_ty, (i as u64) + 1)));
        }
        let call = b.build_call(
            "r",
            i64_ty,
            callee_ty,
            ValueRef::Global(llvm_ir::GlobalId(0)),
            args,
        );
        b.build_ret(call);
        (ctx, module)
    }

    #[test]
    fn win64_call_reserves_shadow_space_and_cleans_stack() {
        let (ctx, mut module) = make_call_fn(6);
        module.target_triple = Some("x86_64-pc-windows-msvc".into());
        let mut be = X86Backend::default();
        let caller = module
            .functions
            .iter()
            .find(|f| f.name == "caller" && !f.is_declaration)
            .expect("caller definition");
        let mf = be.lower_function(&ctx, &module, caller);
        let instrs: Vec<_> = mf.blocks.iter().flat_map(|b| b.instrs.iter()).collect();
        let has_sub32 = instrs.iter().any(|i| {
            i.opcode == SUB_RI
                && i.operands.first() == Some(&MOperand::PReg(crate::regs::RSP))
                && i.operands.get(1) == Some(&MOperand::Imm(32))
        });
        let has_add48 = instrs.iter().any(|i| {
            i.opcode == ADD_RI
                && i.operands.first() == Some(&MOperand::PReg(crate::regs::RSP))
                && i.operands.get(1) == Some(&MOperand::Imm(48))
        });
        let push_count = instrs.iter().filter(|i| i.opcode == PUSH_R).count();
        assert!(has_sub32, "Win64 call must reserve 32-byte shadow space");
        assert!(has_add48, "Win64 call must clean stack args + shadow space");
        assert_eq!(push_count, 2, "6 args => 2 stack-passed arguments");
    }

    #[test]
    fn sysv_call_with_one_stack_arg_adds_alignment_pad() {
        let (ctx, module) = make_call_fn(7);
        let mut be = X86Backend::default();
        let caller = module
            .functions
            .iter()
            .find(|f| f.name == "caller" && !f.is_declaration)
            .expect("caller definition");
        let mf = be.lower_function(&ctx, &module, caller);
        let instrs: Vec<_> = mf.blocks.iter().flat_map(|b| b.instrs.iter()).collect();
        let has_sub8 = instrs.iter().any(|i| {
            i.opcode == SUB_RI
                && i.operands.first() == Some(&MOperand::PReg(crate::regs::RSP))
                && i.operands.get(1) == Some(&MOperand::Imm(8))
        });
        let has_add16 = instrs.iter().any(|i| {
            i.opcode == ADD_RI
                && i.operands.first() == Some(&MOperand::PReg(crate::regs::RSP))
                && i.operands.get(1) == Some(&MOperand::Imm(16))
        });
        let push_count = instrs.iter().filter(|i| i.opcode == PUSH_R).count();
        assert!(
            has_sub8,
            "SysV odd stack args must add 8-byte alignment pad"
        );
        assert!(
            has_add16,
            "SysV cleanup must include stack arg + alignment pad"
        );
        assert_eq!(push_count, 1, "7 args => 1 stack-passed argument");
    }

    #[test]
    fn lower_declaration_is_empty() {
        let mut ctx = Context::new();
        let mut module = Module::new("test");
        let mut b = Builder::new(&mut ctx, &mut module);
        b.add_declaration("ext", b.ctx.void_ty, vec![], false);
        let mut be = X86Backend::default();
        let mf = be.lower_function(&ctx, &module, &module.functions[0]);
        assert!(mf.blocks.is_empty(), "declaration should produce no blocks");
    }

    #[test]
    fn lower_icmp_produces_cmp_and_setcc() {
        let mut ctx = Context::new();
        let mut module = Module::new("test");
        let mut b = Builder::new(&mut ctx, &mut module);
        b.add_function(
            "cmp_fn",
            b.ctx.i1_ty,
            vec![b.ctx.i64_ty, b.ctx.i64_ty],
            vec!["x".into(), "y".into()],
            false,
            Linkage::External,
        );
        let entry = b.add_block("entry");
        b.position_at_end(entry);
        let x = b.get_arg(0);
        let y = b.get_arg(1);
        let cmp = b.build_icmp("cmp", llvm_ir::IntPredicate::Slt, x, y);
        b.build_ret(cmp);

        let mut be = X86Backend::default();
        let mf = be.lower_function(&ctx, &module, &module.functions[0]);

        let has_cmp = mf
            .blocks
            .iter()
            .any(|bl| bl.instrs.iter().any(|i| i.opcode == CMP_RR));
        let has_setcc = mf
            .blocks
            .iter()
            .any(|bl| bl.instrs.iter().any(|i| i.opcode == SETCC));
        assert!(has_cmp, "should emit CMP");
        assert!(has_setcc, "should emit SETCC");
    }

    fn make_div_fn(unsigned: bool) -> (Context, Module) {
        let mut ctx = Context::new();
        let mut module = Module::new("test");
        let mut b = Builder::new(&mut ctx, &mut module);
        b.add_function(
            "div_fn",
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
        let result = if unsigned {
            b.build_udiv("q", a, bv)
        } else {
            b.build_sdiv("q", a, bv)
        };
        b.build_ret(result);
        (ctx, module)
    }

    fn make_mul_const_fn(k: u64) -> (Context, Module) {
        let mut ctx = Context::new();
        let mut module = Module::new("test");
        let mut b = Builder::new(&mut ctx, &mut module);
        b.add_function(
            "mulc_fn",
            b.ctx.i64_ty,
            vec![b.ctx.i64_ty],
            vec!["a".into()],
            false,
            Linkage::External,
        );
        let entry = b.add_block("entry");
        b.position_at_end(entry);
        let a = b.get_arg(0);
        let c = b.const_int(b.ctx.i64_ty, k);
        let m = b.build_mul("m", a, c);
        b.build_ret(m);
        (ctx, module)
    }

    fn make_shl_fn() -> (Context, Module) {
        let mut ctx = Context::new();
        let mut module = Module::new("test");
        let mut b = Builder::new(&mut ctx, &mut module);
        b.add_function(
            "shl_fn",
            b.ctx.i64_ty,
            vec![b.ctx.i64_ty, b.ctx.i64_ty],
            vec!["val".into(), "amt".into()],
            false,
            Linkage::External,
        );
        let entry = b.add_block("entry");
        b.position_at_end(entry);
        let v = b.get_arg(0);
        let a = b.get_arg(1);
        let shifted = b.build_shl("shifted", v, a);
        b.build_ret(shifted);
        (ctx, module)
    }

    #[test]
    fn shl_loads_shift_amount_into_rcx() {
        // Issue #33: shift amount must be moved into RCX (CL) before the shift.
        // Verify: a MOV_PR to RCX appears immediately before SHL_RR, and
        //         the SHL_RR instruction has phys_uses containing RCX.
        use crate::regs::RCX;
        let (ctx, module) = make_shl_fn();
        let mut be = X86Backend::default();
        let mf = be.lower_function(&ctx, &module, &module.functions[0]);

        // There must be a SHL_RR in the function.
        let shl_instr = mf
            .blocks
            .iter()
            .flat_map(|b| b.instrs.iter())
            .find(|i| i.opcode == SHL_RR)
            .expect("should emit SHL_RR");

        // SHL_RR must declare RCX as a physical use (shift reads CL).
        assert!(
            shl_instr.phys_uses.contains(&RCX),
            "SHL_RR must have RCX in phys_uses (CL holds the shift amount)"
        );

        // There must be a MOV_PR targeting RCX somewhere in the function.
        let has_mov_to_rcx = mf.blocks.iter().flat_map(|b| b.instrs.iter()).any(|i| {
            i.opcode == MOV_PR
                && i.operands.first() == Some(&llvm_codegen::isel::MOperand::PReg(RCX))
        });
        assert!(
            has_mov_to_rcx,
            "a MOV_PR loading the shift count into RCX must be emitted before SHL_RR"
        );
    }

    #[test]
    fn udiv_uses_div_r_not_idiv_r() {
        // Issue #31: UDiv must emit DIV_R (unsigned) not IDIV_R (signed).
        let (ctx, module) = make_div_fn(true);
        let mut be = X86Backend::default();
        let mf = be.lower_function(&ctx, &module, &module.functions[0]);
        let has_div_r = mf
            .blocks
            .iter()
            .any(|bl| bl.instrs.iter().any(|i| i.opcode == DIV_R));
        let has_idiv_r = mf
            .blocks
            .iter()
            .any(|bl| bl.instrs.iter().any(|i| i.opcode == IDIV_R));
        assert!(has_div_r, "UDiv must emit DIV_R (unsigned div)");
        assert!(!has_idiv_r, "UDiv must NOT emit IDIV_R (signed div)");
    }

    #[test]
    fn sdiv_uses_idiv_r() {
        // Regression: SDiv must still emit IDIV_R (signed).
        let (ctx, module) = make_div_fn(false);
        let mut be = X86Backend::default();
        let mf = be.lower_function(&ctx, &module, &module.functions[0]);
        let has_idiv_r = mf
            .blocks
            .iter()
            .any(|bl| bl.instrs.iter().any(|i| i.opcode == IDIV_R));
        let has_div_r = mf
            .blocks
            .iter()
            .any(|bl| bl.instrs.iter().any(|i| i.opcode == DIV_R));
        assert!(has_idiv_r, "SDiv must emit IDIV_R (signed div)");
        assert!(!has_div_r, "SDiv must NOT emit DIV_R (unsigned div)");
    }

    #[test]
    fn mul_by_const_uses_imul_rri_pattern() {
        // Isel pattern bridge: `%a * C` should avoid materializing `C` in a
        // register and instead use IMUL_RRI.
        let (ctx, module) = make_mul_const_fn(7);
        let mut be = X86Backend::default();
        let mf = be.lower_function(&ctx, &module, &module.functions[0]);

        let has_imul_rri = mf
            .blocks
            .iter()
            .any(|bl| bl.instrs.iter().any(|i| i.opcode == IMUL_RRI));
        assert!(
            has_imul_rri,
            "mul-by-constant should lower via IMUL_RRI pattern"
        );
    }

    #[test]
    fn emit_mov_to_preg_uses_mov_pr_opcode() {
        // emit_mov_to_preg must use MOV_PR (not MOV_RR) so that the fixed
        // physical register destination survives register allocation.
        // Verify: the instruction has dst=None, opcode=MOV_PR,
        //         operands[0]=PReg(preg), operands[1]=VReg(src).
        use crate::regs::RAX;
        use llvm_codegen::isel::{MOperand, MachineFunction};

        let mut mf = MachineFunction::new("f".into());
        let b = mf.add_block("entry");
        let src = mf.fresh_vreg();
        super::emit_mov_to_preg(&mut mf, b, RAX, src);

        let instr = &mf.blocks[b].instrs[0];
        assert_eq!(
            instr.opcode, MOV_PR,
            "emit_mov_to_preg must use MOV_PR opcode"
        );
        assert!(
            instr.dst.is_none(),
            "dst must be None (destination is a fixed PReg)"
        );
        assert_eq!(instr.operands.len(), 2);
        assert_eq!(
            instr.operands[0],
            MOperand::PReg(RAX),
            "operands[0] must be the fixed PReg"
        );
        assert_eq!(
            instr.operands[1],
            MOperand::VReg(src),
            "operands[1] must be the source VReg"
        );
    }

    #[test]
    fn add_binop_instr_has_single_rhs_operand() {
        // After fix for issue #34: the ADD_RR instruction must have exactly
        // one operand (the RHS vreg), not two (self-reference + rhs).
        let (ctx, module) = make_add_fn();
        let mut be = X86Backend::default();
        let mf = be.lower_function(&ctx, &module, &module.functions[0]);

        // Find the ADD_RR instruction.
        let add_instr = mf
            .blocks
            .iter()
            .flat_map(|b| b.instrs.iter())
            .find(|i| i.opcode == ADD_RR);
        let add_instr = add_instr.expect("should have an ADD_RR instruction");

        assert_eq!(
            add_instr.operands.len(),
            1,
            "ADD_RR must carry only the RHS operand, not a self-reference (issue #34)"
        );
    }

    fn make_sext_fn(src_ty_bits: u32) -> (Context, Module) {
        let mut ctx = Context::new();
        let mut module = Module::new("test");
        let src_ty = ctx.mk_int(src_ty_bits);
        let mut b = Builder::new(&mut ctx, &mut module);
        b.add_function(
            "sext_fn",
            b.ctx.i64_ty,
            vec![src_ty],
            vec!["x".into()],
            false,
            Linkage::External,
        );
        let entry = b.add_block("entry");
        b.position_at_end(entry);
        let x = b.get_arg(0);
        let ext = b.build_sext("ext", x, b.ctx.i64_ty);
        b.build_ret(ext);
        (ctx, module)
    }

    #[test]
    fn sext_i8_uses_movsx_8() {
        let (ctx, module) = make_sext_fn(8);
        let mut be = X86Backend::default();
        let mf = be.lower_function(&ctx, &module, &module.functions[0]);
        let has_movsx8 = mf
            .blocks
            .iter()
            .any(|b| b.instrs.iter().any(|i| i.opcode == MOVSX_8));
        assert!(
            has_movsx8,
            "sext from i8 must use MOVSX_8 (0F BE), not MOVSXD (63)"
        );
    }

    #[test]
    fn sext_i16_uses_movsx_16() {
        let (ctx, module) = make_sext_fn(16);
        let mut be = X86Backend::default();
        let mf = be.lower_function(&ctx, &module, &module.functions[0]);
        let has_movsx16 = mf
            .blocks
            .iter()
            .any(|b| b.instrs.iter().any(|i| i.opcode == MOVSX_16));
        assert!(
            has_movsx16,
            "sext from i16 must use MOVSX_16 (0F BF), not MOVSXD (63)"
        );
    }

    #[test]
    fn sext_i32_uses_movsx_32() {
        let (ctx, module) = make_sext_fn(32);
        let mut be = X86Backend::default();
        let mf = be.lower_function(&ctx, &module, &module.functions[0]);
        let has_movsx32 = mf
            .blocks
            .iter()
            .any(|b| b.instrs.iter().any(|i| i.opcode == MOVSX_32));
        assert!(has_movsx32, "sext from i32 must use MOVSX_32 (movsxd, 63)");
    }

    /// Build a function with a CondBr where each successor has a phi:
    ///
    /// ```llvm
    /// define i64 @phi_test(i64 %a, i64 %b, i1 %cond) {
    /// entry:
    ///   br i1 %cond, label %then_bb, label %else_bb
    /// then_bb:
    ///   %px = phi i64 [ %a, %entry ]
    ///   br label %merge
    /// else_bb:
    ///   %py = phi i64 [ %b, %entry ]
    ///   br label %merge
    /// merge:
    ///   %r = phi i64 [ %px, %then_bb ], [ %py, %else_bb ]
    ///   ret i64 %r
    /// }
    /// ```
    fn make_condbr_phi_fn() -> (Context, Module) {
        let mut ctx = Context::new();
        let mut module = Module::new("test");
        let mut b = Builder::new(&mut ctx, &mut module);
        b.add_function(
            "phi_test",
            b.ctx.i64_ty,
            vec![b.ctx.i64_ty, b.ctx.i64_ty, b.ctx.i1_ty],
            vec!["a".into(), "b".into(), "cond".into()],
            false,
            Linkage::External,
        );
        // entry: br i1 %cond, label %then_bb, label %else_bb
        let entry = b.add_block("entry");
        let then_bb = b.add_block("then_bb");
        let else_bb = b.add_block("else_bb");
        let merge_bb = b.add_block("merge");

        b.position_at_end(entry);
        let a = b.get_arg(0);
        let bv = b.get_arg(1);
        let cond = b.get_arg(2);
        b.build_cond_br(cond, then_bb, else_bb);

        // then_bb: phi from %a (entry)
        b.position_at_end(then_bb);
        let px = b.build_phi("px", b.ctx.i64_ty, vec![(a, entry)]);
        b.build_br(merge_bb);

        // else_bb: phi from %b (entry)
        b.position_at_end(else_bb);
        let py = b.build_phi("py", b.ctx.i64_ty, vec![(bv, entry)]);
        b.build_br(merge_bb);

        // merge: phi from both paths, then ret
        b.position_at_end(merge_bb);
        let r = b.build_phi("r", b.ctx.i64_ty, vec![(px, then_bb), (py, else_bb)]);
        b.build_ret(r);

        (ctx, module)
    }

    #[test]
    fn condbr_phi_copies_use_edge_split_blocks() {
        // After the fix, each CondBr edge gets its own trampoline machine block
        // for phi copies.  The machine function must have more blocks than the
        // IR function (one trampoline per conditional edge).
        let (ctx, module) = make_condbr_phi_fn();
        let func = &module.functions[0];
        let ir_block_count = func.blocks.len(); // entry, then_bb, else_bb, merge = 4

        let mut be = X86Backend::default();
        let mf = be.lower_function(&ctx, &module, func);

        assert!(
            mf.blocks.len() > ir_block_count,
            "CondBr must create trampoline edge blocks for phi copies; \
             expected > {} machine blocks, got {}",
            ir_block_count,
            mf.blocks.len()
        );
    }

    #[test]
    fn condbr_predecessor_jumps_to_trampolines_not_successors() {
        // The CondBr predecessor block must branch to the trampoline blocks
        // (indices ≥ ir_block_count), not directly to the IR successor blocks.
        let (ctx, module) = make_condbr_phi_fn();
        let func = &module.functions[0];
        let ir_block_count = func.blocks.len(); // 4

        let mut be = X86Backend::default();
        let mf = be.lower_function(&ctx, &module, func);

        // entry is machine block 0; find its JCC and JMP targets.
        let entry_block = &mf.blocks[0];
        let branch_targets: Vec<usize> = entry_block
            .instrs
            .iter()
            .filter_map(|i| {
                if i.opcode == JCC || i.opcode == JMP {
                    i.operands.iter().find_map(|op| {
                        if let MOperand::Block(b) = op {
                            Some(*b)
                        } else {
                            None
                        }
                    })
                } else {
                    None
                }
            })
            .collect();

        assert!(
            !branch_targets.is_empty(),
            "entry block should have JCC/JMP branch instructions"
        );
        for &target in &branch_targets {
            assert!(
                target >= ir_block_count,
                "CondBr must branch to trampoline blocks (index >= {}), got target {}",
                ir_block_count,
                target
            );
        }
    }

    #[test]
    fn constants_are_materialized_in_non_dominated_blocks() {
        // Regression for issue #106:
        // if a constant is first used in `then` and reused in `merge`, `merge`
        // must materialize it locally rather than reusing a VReg from `then`.
        let mut ctx = Context::new();
        let mut module = Module::new("test");
        let mut b = Builder::new(&mut ctx, &mut module);
        b.add_function(
            "const_dom",
            b.ctx.i64_ty,
            vec![b.ctx.i1_ty],
            vec!["cond".into()],
            false,
            Linkage::External,
        );
        let entry = b.add_block("entry");
        let then_bb = b.add_block("then_bb");
        let else_bb = b.add_block("else_bb");
        let merge_bb = b.add_block("merge_bb");

        b.position_at_end(entry);
        let cond = b.get_arg(0);
        b.build_cond_br(cond, then_bb, else_bb);

        b.position_at_end(then_bb);
        let c40 = b.const_int(b.ctx.i64_ty, 40);
        let c1 = b.const_int(b.ctx.i64_ty, 1);
        let from_then = b.build_add("from_then", c40, c1);
        b.build_br(merge_bb);

        b.position_at_end(else_bb);
        b.build_br(merge_bb);

        b.position_at_end(merge_bb);
        let c0 = b.const_int(b.ctx.i64_ty, 0);
        let phi = b.build_phi(
            "phi",
            b.ctx.i64_ty,
            vec![(from_then, then_bb), (c0, else_bb)],
        );
        let plus_one = b.build_add("plus_one", phi, c1);
        b.build_ret(plus_one);

        let mut be = X86Backend::default();
        let mf = be.lower_function(&ctx, &module, &module.functions[0]);

        // merge_bb is IR block index 3, so machine block 3 before edge splits.
        let merge = &mf.blocks[3];
        let has_local_imm1 = merge.instrs.iter().any(|mi| {
            mi.opcode == MOV_RI && mi.operands.iter().any(|op| matches!(op, MOperand::Imm(1)))
        });
        assert!(
            has_local_imm1,
            "merge block must materialize constant 1 locally (cannot reuse branch-local VReg)"
        );
    }

    fn make_vector_ops_fn() -> (Context, Module) {
        let mut ctx = Context::new();
        let mut module = Module::new("vec");
        let mut b = Builder::new(&mut ctx, &mut module);
        let v4i32 = b.ctx.mk_vector(b.ctx.i32_ty, 4, false);
        b.add_function(
            "vec_fn",
            b.ctx.i32_ty,
            vec![v4i32],
            vec!["v".into()],
            false,
            Linkage::External,
        );
        let entry = b.add_block("entry");
        b.position_at_end(entry);
        let v = b.get_arg(0);
        let idx0 = b.const_int(b.ctx.i32_ty, 0);
        let idx1 = b.const_int(b.ctx.i32_ty, 1);
        let lane0 = b.build_extractelement("lane0", v, idx0, b.ctx.i32_ty);
        let v2 = b.build_insertelement("v2", v, lane0, idx1);
        let zv = ValueRef::Constant(b.ctx.const_zero(v4i32));
        let v3 = b.build_shufflevector("v3", v2, zv, vec![0, 1, 4, 5], v4i32);
        let lane1 = b.build_extractelement("lane1", v3, idx1, b.ctx.i32_ty);
        b.build_ret(lane1);
        (ctx, module)
    }

    fn make_vec_add_i32_fn() -> (Context, Module) {
        let mut ctx = Context::new();
        let mut module = Module::new("vec_i32");
        let mut b = Builder::new(&mut ctx, &mut module);
        let v4i32 = b.ctx.mk_vector(b.ctx.i32_ty, 4, false);
        b.add_function(
            "main",
            b.ctx.i32_ty,
            vec![],
            vec![],
            false,
            Linkage::External,
        );
        let entry = b.add_block("entry");
        b.position_at_end(entry);
        let z = ValueRef::Constant(b.ctx.const_zero(v4i32));
        let s = b.build_add("s", z, z);
        let i0 = ValueRef::Constant(b.ctx.const_int(b.ctx.i32_ty, 0));
        let lane0 = b.build_extractelement("lane0", s, i0, b.ctx.i32_ty);
        b.build_ret(lane0);
        (ctx, module)
    }

    fn make_vec_fadd_f32_fn() -> (Context, Module) {
        let mut ctx = Context::new();
        let mut module = Module::new("vec_f32");
        let mut b = Builder::new(&mut ctx, &mut module);
        let v4f32 = b.ctx.mk_vector(b.ctx.f32_ty, 4, false);
        b.add_function(
            "main",
            b.ctx.i32_ty,
            vec![],
            vec![],
            false,
            Linkage::External,
        );
        let entry = b.add_block("entry");
        b.position_at_end(entry);
        let z = ValueRef::Constant(b.ctx.const_zero(v4f32));
        let s = b.build_fadd("s", z, z);
        let i0 = ValueRef::Constant(b.ctx.const_int(b.ctx.i32_ty, 0));
        let lane0 = b.build_extractelement("lane0", s, i0, b.ctx.i32_ty);
        b.build_ret(lane0);
        (ctx, module)
    }

    fn make_vec_add_i32_len_fn(len: u32) -> (Context, Module) {
        let mut ctx = Context::new();
        let mut module = Module::new("vec_i32_len");
        let mut b = Builder::new(&mut ctx, &mut module);
        let vti = b.ctx.mk_vector(b.ctx.i32_ty, len, false);
        b.add_function(
            "main",
            b.ctx.i32_ty,
            vec![],
            vec![],
            false,
            Linkage::External,
        );
        let entry = b.add_block("entry");
        b.position_at_end(entry);
        let z = ValueRef::Constant(b.ctx.const_zero(vti));
        let s = b.build_add("s", z, z);
        let i0 = ValueRef::Constant(b.ctx.const_int(b.ctx.i32_ty, 0));
        let lane0 = b.build_extractelement("lane0", s, i0, b.ctx.i32_ty);
        b.build_ret(lane0);
        (ctx, module)
    }

    #[test]
    fn vector_i32_add_uses_paddd_when_sse42_enabled() {
        let (ctx, module) = make_vec_add_i32_fn();
        let mut be = X86Backend::new(TargetFeatures::sse42());
        let mf = be.lower_function(&ctx, &module, &module.functions[0]);
        assert!(
            mf.blocks
                .iter()
                .flat_map(|b| &b.instrs)
                .any(|i| i.opcode == PADDD_RR),
            "expected PADDD lowering for <4 x i32> add under SSE4.2"
        );
    }

    #[test]
    fn vector_f32_add_uses_addps_when_sse42_enabled() {
        let (ctx, module) = make_vec_fadd_f32_fn();
        let mut be = X86Backend::new(TargetFeatures::sse42());
        let mf = be.lower_function(&ctx, &module, &module.functions[0]);
        assert!(
            mf.blocks
                .iter()
                .flat_map(|b| &b.instrs)
                .any(|i| i.opcode == ADDPS_RR),
            "expected ADDPS lowering for <4 x float> fadd under SSE4.2"
        );
    }

    #[test]
    fn vector_lowering_does_not_panic_with_baseline_features() {
        let (ctx, module) = make_vector_ops_fn();
        let mut be = X86Backend::new(TargetFeatures::baseline());
        let mf = be.lower_function(&ctx, &module, &module.functions[0]);
        assert!(
            mf.blocks.iter().any(|b| !b.instrs.is_empty()),
            "vector fallback lowering should still emit machine instructions"
        );
    }

    #[test]
    fn vector_lowering_does_not_panic_with_sse42_enabled() {
        let (ctx, module) = make_vector_ops_fn();
        let mut be = X86Backend::new(TargetFeatures::sse42());
        let mf = be.lower_function(&ctx, &module, &module.functions[0]);
        assert!(
            mf.blocks.iter().any(|b| !b.instrs.is_empty()),
            "SSE4.2 feature gate path should lower vector instructions"
        );
    }

    #[test]
    fn vector_lowering_does_not_panic_with_avx2_enabled() {
        let (ctx, module) = make_vector_ops_fn();
        let mut be = X86Backend::new(TargetFeatures::avx2());
        let mf = be.lower_function(&ctx, &module, &module.functions[0]);
        assert!(
            mf.blocks.iter().any(|b| !b.instrs.is_empty()),
            "AVX2 feature gate path should lower vector instructions"
        );
    }

    #[test]
    fn vector_i32x16_add_uses_simd_path_when_avx512f_enabled() {
        let (ctx, module) = make_vec_add_i32_len_fn(16);
        let mut be = X86Backend::new(TargetFeatures::avx512f());
        let mf = be.lower_function(&ctx, &module, &module.functions[0]);
        assert!(
            mf.blocks
                .iter()
                .flat_map(|b| &b.instrs)
                .any(|i| i.opcode == PADDD_RR),
            "expected SIMD lowering for <16 x i32> add under AVX-512F gate"
        );
    }

    #[test]
    fn vector_i32x16_add_falls_back_without_avx512f() {
        let (ctx, module) = make_vec_add_i32_len_fn(16);
        let mut be = X86Backend::new(TargetFeatures::avx2());
        let mf = be.lower_function(&ctx, &module, &module.functions[0]);
        assert!(
            mf.blocks
                .iter()
                .flat_map(|b| &b.instrs)
                .all(|i| i.opcode != PADDD_RR),
            "without AVX-512F, <16 x i32> should avoid AVX-512-gated SIMD opcode path"
        );
    }
}
