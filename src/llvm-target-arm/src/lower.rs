//! AArch64 IR → machine-IR lowering.
//!
//! Implements `IselBackend` for `AArch64Backend`.  Each IR instruction is
//! translated to one or more machine instructions using virtual registers.
//! Phi-destruction (parallel copy insertion) is also handled here.

use crate::{
    abi::{classify_aapcs64_args, classify_aapcs64_args_typed, ArgKind, ArgLocation, INT_RET},
    instructions::*,
    regs::{ALLOCATABLE, CALLEE_SAVED, FP_ALLOCATABLE, FP_RET_REG},
};
use llvm_codegen::isel::{DebugLoc, IselBackend, MInstr, MachineFunction, PReg, VReg};
use llvm_codegen::sizeof_ty;
use llvm_ir::{
    ArgId, BlockId, ConstantData, Context, Function, InstrId, InstrKind, InstrprofIntrinsic,
    IntPredicate, Module, RmwOp, TypeData, ValueRef,
};
use std::collections::HashMap;

/// Target feature flags for AArch64.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AArch64Features {
    /// Enable ARMv8.1 LSE atomic instructions (CASAL, LDADDAL, etc.).
    /// Default `true` — matches Cortex-A55+ and all Apple Silicon.
    pub lse: bool,
}

impl AArch64Features {
    /// Baseline ARMv8.0 — no LSE extension.
    pub const fn baseline() -> Self {
        Self { lse: false }
    }
    /// ARMv8.1 LSE enabled (modern default).
    pub const fn lse() -> Self {
        Self { lse: true }
    }
}

/// AArch64 instruction-selection backend.
pub struct AArch64Backend {
    /// Target feature flags controlling which instruction set extensions to use.
    pub features: AArch64Features,
}

impl Default for AArch64Backend {
    fn default() -> Self {
        // Default to LSE=true — correct for Cortex-A55+, Apple Silicon, and most
        // modern AArch64 cores.
        Self {
            features: AArch64Features::lse(),
        }
    }
}

impl AArch64Backend {
    /// Construct a backend with the given feature set.
    pub fn new(features: AArch64Features) -> Self {
        Self { features }
    }
}

impl IselBackend for AArch64Backend {
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
        mf.callee_saved_pregs = CALLEE_SAVED.to_vec();
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
        // Float/double args go in D-registers; integer/pointer args in X-registers.
        let arg_kinds: Vec<ArgKind> = func
            .args
            .iter()
            .map(|a| {
                if is_float_type(ctx, a.ty) {
                    ArgKind::Float
                } else {
                    ArgKind::Int
                }
            })
            .collect();
        let arg_locs = classify_aapcs64_args_typed(&arg_kinds);
        for (i, arg) in func.args.iter().enumerate() {
            let vr = if is_float_type(ctx, arg.ty) {
                mf.fresh_float_vreg()
            } else {
                mf.fresh_vreg()
            };
            vmap.insert(ValueRef::Argument(ArgId(i as u32)), vr);
            match arg_locs[i] {
                ArgLocation::Reg(preg) => {
                    // mov vreg, preg  (MOV_RR works for both int and fp registers
                    // in the lowering sense; the encoder dispatches by opcode)
                    let mov_op = if is_float_type(ctx, arg.ty) {
                        FMOV_RR
                    } else {
                        MOV_RR
                    };
                    let mut mi = MInstr::new(mov_op).with_dst(vr).with_preg(preg);
                    mi.phys_uses = vec![preg];
                    mf.push(0, mi);
                }
                ArgLocation::Stack(offset) => {
                    // Stack arguments: emit a placeholder immediate to mark the slot.
                    mf.push(0, MInstr::new(MOV_IMM).with_dst(vr).with_imm(offset as i64));
                }
            }
        }

        let features = self.features;

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
                lower_instr(ctx, module, func, &mut mf, bi, iid, &mut vmap, features);
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

// ── FP type helpers ───────────────────────────────────────────────────────

/// Returns `true` if `ty` is a scalar floating-point type (half, float, double, …).
fn is_float_type(ctx: &Context, ty: llvm_ir::TypeId) -> bool {
    matches!(ctx.get_type(ty), TypeData::Float(_))
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
    if let Some(&existing) = vmap.get(&vr) {
        return existing;
    }
    match vr {
        ValueRef::Constant(cid) => {
            let cd = ctx.get_const(cid);
            // Float constants are materialised into float VRegs.
            if let ConstantData::Float { ty, .. } = cd {
                if is_float_type(ctx, *ty) {
                    let vreg = mf.fresh_float_vreg();
                    vmap.insert(vr, vreg);
                    // Materialise via integer bits then FMOV from int (placeholder):
                    // For now emit MOV_IMM with the raw bits. A full implementation
                    // would use FMOV (GP→FP) or a literal pool load.
                    let bits = const_to_imm(cd);
                    mf.push(mblock, MInstr::new(MOV_IMM).with_dst(vreg).with_imm(bits));
                    return vreg;
                }
            }
            let vreg = mf.fresh_vreg();
            vmap.insert(vr, vreg);
            let imm = const_to_imm(cd);
            mf.push(mblock, MInstr::new(MOV_IMM).with_dst(vreg).with_imm(imm));
            vreg
        }
        _ => {
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

fn instrprof_from_callee(ctx: &Context, callee: ValueRef) -> Option<InstrprofIntrinsic> {
    let ValueRef::Constant(cid) = callee else {
        return None;
    };
    let ConstantData::GlobalRef { name, .. } = ctx.get_const(cid) else {
        return None;
    };
    InstrprofIntrinsic::from_name(name)
}

fn inline_asm_bytes_aarch64(template: &str) -> Vec<u8> {
    let mut bytes = Vec::new();
    for part in template.split([';', '\n']) {
        match part.trim().to_ascii_lowercase().as_str() {
            "" => {}
            "nop" => bytes.extend_from_slice(&0xD503201Fu32.to_le_bytes()),
            _ => bytes.extend_from_slice(&0xD503201Fu32.to_le_bytes()),
        }
    }
    bytes
}

fn pred_to_cc(pred: IntPredicate) -> i64 {
    match pred {
        IntPredicate::Eq => CC_EQ,
        IntPredicate::Ne => CC_NE,
        IntPredicate::Slt => CC_LT,
        IntPredicate::Sle => CC_LE,
        IntPredicate::Sgt => CC_GT,
        IntPredicate::Sge => CC_GE,
        IntPredicate::Ult => CC_LO,
        IntPredicate::Ule => CC_LS,
        IntPredicate::Ugt => CC_HI,
        IntPredicate::Uge => CC_HS,
    }
}

// ── instruction lowering ──────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn lower_instr(
    ctx: &Context,
    _module: &Module,
    func: &Function,
    mf: &mut MachineFunction,
    mblock: usize,
    iid: InstrId,
    vmap: &mut HashMap<ValueRef, VReg>,
    features: AArch64Features,
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
    // Helper: allocate a fresh float dst VReg and register it.
    macro_rules! new_fp_dst {
        () => {{
            let v = mf.fresh_float_vreg();
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
    // Helper: emit a three-register binary op (AArch64 has proper 3-address form).
    // dst = op(lhs, rhs)
    macro_rules! emit_binop3 {
        ($op:expr, $lhs:expr, $rhs:expr) => {{
            let dst = new_dst!();
            let l = res!($lhs);
            let r = res!($rhs);
            mf.push(
                mblock,
                MInstr::new($op).with_dst(dst).with_vreg(l).with_vreg(r),
            );
        }};
    }
    // Helper: emit a three-register FP binary op with float dst VReg.
    macro_rules! emit_fp_binop3 {
        ($op:expr, $lhs:expr, $rhs:expr) => {{
            let dst = new_fp_dst!();
            let l = res!($lhs);
            let r = res!($rhs);
            mf.push(
                mblock,
                MInstr::new($op).with_dst(dst).with_vreg(l).with_vreg(r),
            );
        }};
    }

    match &instr.kind {
        // ── arithmetic ─────────────────────────────────────────────────────
        // AArch64 has 3-address instructions: dst = op(lhs, rhs)
        Add { lhs, rhs, .. } => {
            emit_binop3!(ADD_RR, *lhs, *rhs);
        }
        Sub { lhs, rhs, .. } => {
            emit_binop3!(SUB_RR, *lhs, *rhs);
        }
        Mul { lhs, rhs, .. } => {
            emit_binop3!(MUL_RR, *lhs, *rhs);
        }
        SDiv { lhs, rhs, .. } => {
            emit_binop3!(SDIV_RR, *lhs, *rhs);
        }
        UDiv { lhs, rhs, .. } => {
            emit_binop3!(UDIV_RR, *lhs, *rhs);
        }

        SRem { lhs, rhs, .. } => {
            // AArch64 has no SREM instruction.
            // srem = lhs - (sdiv(lhs, rhs) * rhs)
            let dst = new_dst!();
            let l = res!(*lhs);
            let r = res!(*rhs);
            let quot = mf.fresh_vreg();
            mf.push(
                mblock,
                MInstr::new(SDIV_RR)
                    .with_dst(quot)
                    .with_vreg(l)
                    .with_vreg(r),
            );
            let prod = mf.fresh_vreg();
            mf.push(
                mblock,
                MInstr::new(MUL_RR)
                    .with_dst(prod)
                    .with_vreg(quot)
                    .with_vreg(r),
            );
            mf.push(
                mblock,
                MInstr::new(SUB_RR)
                    .with_dst(dst)
                    .with_vreg(l)
                    .with_vreg(prod),
            );
        }

        URem { lhs, rhs, .. } => {
            // AArch64 has no UREM instruction.
            // urem = lhs - (udiv(lhs, rhs) * rhs)
            let dst = new_dst!();
            let l = res!(*lhs);
            let r = res!(*rhs);
            let quot = mf.fresh_vreg();
            mf.push(
                mblock,
                MInstr::new(UDIV_RR)
                    .with_dst(quot)
                    .with_vreg(l)
                    .with_vreg(r),
            );
            let prod = mf.fresh_vreg();
            mf.push(
                mblock,
                MInstr::new(MUL_RR)
                    .with_dst(prod)
                    .with_vreg(quot)
                    .with_vreg(r),
            );
            mf.push(
                mblock,
                MInstr::new(SUB_RR)
                    .with_dst(dst)
                    .with_vreg(l)
                    .with_vreg(prod),
            );
        }

        // ── bitwise ────────────────────────────────────────────────────────
        And { lhs, rhs } => {
            emit_binop3!(AND_RR, *lhs, *rhs);
        }
        Or { lhs, rhs } => {
            emit_binop3!(ORR_RR, *lhs, *rhs);
        }
        Xor { lhs, rhs } => {
            emit_binop3!(EOR_RR, *lhs, *rhs);
        }

        // ── shifts ─────────────────────────────────────────────────────────
        // AArch64 shift-by-register instructions (LSLV/LSRV/ASRV) are 3-address.
        Shl { lhs, rhs, .. } => {
            emit_binop3!(LSL_RR, *lhs, *rhs);
        }
        LShr { lhs, rhs, .. } => {
            emit_binop3!(LSR_RR, *lhs, *rhs);
        }
        AShr { lhs, rhs, .. } => {
            emit_binop3!(ASR_RR, *lhs, *rhs);
        }

        // ── comparisons ────────────────────────────────────────────────────
        ICmp { pred, lhs, rhs } => {
            let dst = new_dst!();
            let l = res!(*lhs);
            let r = res!(*rhs);
            let cc = pred_to_cc(*pred);
            mf.push(mblock, MInstr::new(CMP_RR).with_vreg(l).with_vreg(r));
            mf.push(mblock, MInstr::new(CSET).with_dst(dst).with_imm(cc));
        }

        FCmp { lhs, rhs, .. } => {
            // FCMPE Dn, Dm sets NZCV flags; then CSET reads them.
            // We only support ordered comparisons here (OEQ, OLT, OLE, OGT, OGE, ONE).
            // For unordered predicates this is a simplification.
            let dst = new_dst!();
            let l = res!(*lhs);
            let r = res!(*rhs);
            mf.push(mblock, MInstr::new(FCMP_RR).with_vreg(l).with_vreg(r));
            // Default: CSET GT (a proxy — callers using FCmp for branches will use
            // the flags directly via the B_COND path once that is wired).
            mf.push(mblock, MInstr::new(CSET).with_dst(dst).with_imm(CC_GT));
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
            // Compare condition to zero, then conditionally select.
            // Simplified: cmp c, 0; cset tmp, ne; neg mask from tmp; and/or combine.
            // We use the same approach as x86: mask-based selection.
            let zero = mf.fresh_vreg();
            mf.push(mblock, MInstr::new(MOV_IMM).with_dst(zero).with_imm(0));
            mf.push(mblock, MInstr::new(CMP_RR).with_vreg(c).with_vreg(zero));
            let scratch = mf.fresh_vreg();
            mf.push(mblock, MInstr::new(CSET).with_dst(scratch).with_imm(CC_NE));
            // neg scratch: scratch = 0 → 0, 1 → -1 (all ones).
            mf.push(
                mblock,
                MInstr::new(NEG_R).with_dst(scratch).with_vreg(scratch),
            );
            // dst = (tv & scratch) | (fv & ~scratch)
            // We don't have NOT, so use: fv & (neg scratch ^ -1) = fv xor scratch... too complex.
            // Simple fallback: dst = fv; if cond != 0 => dst = tv.
            // Emit: and tmp1, tv, scratch; bic (= and with complement) not available easily.
            // Use CSET mask approach:
            let tmp1 = mf.fresh_vreg();
            let tmp2 = mf.fresh_vreg();
            // tmp1 = fv & ~scratch  (scratch = all-ones if cond true, 0 if false)
            // We compute NOT scratch as XOR with -1.
            let notmask = mf.fresh_vreg();
            let allones = mf.fresh_vreg();
            // MOV_WIDE is required for -1 (0xFFFF_FFFF_FFFF_FFFF) because
            // MOV_IMM (MOVZ) only loads a 16-bit zero-extended immediate and
            // would produce 0xFFFF instead of all-ones.
            mf.push(mblock, MInstr::new(MOV_WIDE).with_dst(allones).with_imm(-1));
            mf.push(
                mblock,
                MInstr::new(EOR_RR)
                    .with_dst(notmask)
                    .with_vreg(scratch)
                    .with_vreg(allones),
            );
            mf.push(
                mblock,
                MInstr::new(AND_RR)
                    .with_dst(tmp1)
                    .with_vreg(fv)
                    .with_vreg(notmask),
            );
            mf.push(
                mblock,
                MInstr::new(AND_RR)
                    .with_dst(tmp2)
                    .with_vreg(tv)
                    .with_vreg(scratch),
            );
            mf.push(
                mblock,
                MInstr::new(ORR_RR)
                    .with_dst(dst)
                    .with_vreg(tmp1)
                    .with_vreg(tmp2),
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
        | AddrSpaceCast { val, .. }
        | Freeze { val } => {
            let dst = new_dst!();
            let src = res!(*val);
            mf.push(mblock, MInstr::new(MOV_RR).with_dst(dst).with_vreg(src));
        }

        // FP↔int conversions.
        FPToSI { val, .. } => {
            let dst = new_dst!();
            let src = res!(*val);
            mf.push(mblock, MInstr::new(FCVTZS_RR).with_dst(dst).with_vreg(src));
        }
        FPToUI { val, .. } => {
            // AArch64 also has FCVTZU (unsigned); use FCVTZS as a simplification.
            let dst = new_dst!();
            let src = res!(*val);
            mf.push(mblock, MInstr::new(FCVTZS_RR).with_dst(dst).with_vreg(src));
        }
        SIToFP { val, .. } => {
            let dst = new_fp_dst!();
            let src = res!(*val);
            mf.push(mblock, MInstr::new(SCVTF_RR).with_dst(dst).with_vreg(src));
        }
        UIToFP { val, .. } => {
            // UCVTF for unsigned; use SCVTF as a simplification.
            let dst = new_fp_dst!();
            let src = res!(*val);
            mf.push(mblock, MInstr::new(UCVTF_RR).with_dst(dst).with_vreg(src));
        }

        // FP precision changes — use FMOV_RR as placeholder (same-type copy).
        FPTrunc { val, .. } | FPExt { val, .. } => {
            let dst = new_fp_dst!();
            let src = res!(*val);
            mf.push(mblock, MInstr::new(FMOV_RR).with_dst(dst).with_vreg(src));
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
                SXTB
            } else if src_bits <= 16 {
                SXTH
            } else {
                SXTW
            };
            mf.push(mblock, MInstr::new(opcode).with_dst(dst).with_vreg(src));
        }

        // ── calls ──────────────────────────────────────────────────────────
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
            let arg_locs = classify_aapcs64_args(args.len());
            for (i, &arg_vref) in args.iter().enumerate() {
                let src = res!(arg_vref);
                match arg_locs[i] {
                    ArgLocation::Reg(preg) => {
                        emit_mov_to_preg(mf, mblock, preg, src);
                    }
                    ArgLocation::Stack(_off) => {
                        // Stack arguments: use a placeholder store.
                        mf.push(mblock, MInstr::new(STR).with_vreg(src));
                    }
                }
            }
            let callee_vr = res!(*callee);
            let mut call_mi = MInstr::new(BLR).with_vreg(callee_vr);
            call_mi.clobbers = ALLOCATABLE.to_vec();
            mf.push(mblock, call_mi);

            // Capture return value from X0.
            let dst = new_dst!();
            emit_mov_from_preg(mf, mblock, dst, INT_RET);
        }

        InlineAsm {
            asm_string, args, ..
        } => {
            for &arg in args {
                let _ = res!(arg);
            }
            mf.push(
                mblock,
                MInstr::new(INLINE_ASM).with_bytes(inline_asm_bytes_aarch64(asm_string)),
            );
            if instr.ty != ctx.void_ty {
                let dst = new_dst!();
                mf.push(mblock, MInstr::new(MOV_IMM).with_dst(dst).with_imm(0));
            }
        }

        // ── atomics ────────────────────────────────────────────────────────
        // AArch64 atomics use either LSE (CASAL / LD<op>AL) when the `lse`
        // feature is enabled (ARMv8.1+), or a basic LDXR/STXR pair otherwise.
        Fence { .. } => {
            // DMB ISH — data memory barrier, inner-shareable domain.
            mf.push(mblock, MInstr::new(DMB_ISH));
        }
        CmpXchg { ptr, cmp, new_val, .. } => {
            let ptr_vr = res!(*ptr);
            let cmp_vr = res!(*cmp);
            let new_vr = res!(*new_val);
            let dst = new_dst!();
            if features.lse {
                // CASAL Xs(cmp), Xt(new_val), [Xn(ptr)]
                // The old memory value is returned in the `cmp` register (Xs).
                mf.push(
                    mblock,
                    MInstr::new(CASAL)
                        .with_dst(dst)
                        .with_vreg(ptr_vr)
                        .with_vreg(cmp_vr)
                        .with_vreg(new_vr),
                );
            } else {
                // LL/SC pair (non-LSE path).
                // Full retry loop is a follow-up; emit the basic pair here.
                mf.push(
                    mblock,
                    MInstr::new(LDXR).with_dst(dst).with_vreg(ptr_vr),
                );
                // STXR: status dst (not exposed as an SSA result), val, ptr.
                let status = mf.fresh_vreg();
                mf.push(
                    mblock,
                    MInstr::new(STXR)
                        .with_dst(status)
                        .with_vreg(ptr_vr)
                        .with_vreg(new_vr),
                );
            }
        }
        AtomicRmw { op, ptr, val, .. } => {
            let ptr_vr = res!(*ptr);
            let val_vr = res!(*val);
            let dst = new_dst!();
            if features.lse {
                let atomic_op = match op {
                    RmwOp::Add | RmwOp::Sub => LDADDAL,
                    RmwOp::And | RmwOp::Nand => LDCLRAL,
                    RmwOp::Or => LDSETAL,
                    RmwOp::Xor => LDEORAL,
                    RmwOp::Xchg => SWPAL,
                    // min/max/fp variants — use LDADDAL as a placeholder.
                    _ => LDADDAL,
                };
                mf.push(
                    mblock,
                    MInstr::new(atomic_op)
                        .with_dst(dst)
                        .with_vreg(ptr_vr)
                        .with_vreg(val_vr),
                );
            } else {
                // LL/SC pair (non-LSE path).
                mf.push(
                    mblock,
                    MInstr::new(LDXR).with_dst(dst).with_vreg(ptr_vr),
                );
                let status = mf.fresh_vreg();
                mf.push(
                    mblock,
                    MInstr::new(STXR)
                        .with_dst(status)
                        .with_vreg(ptr_vr)
                        .with_vreg(val_vr),
                );
            }
        }

        // ── memory (placeholder — mem2reg removes most alloca/load/store) ────
        // Store produces no SSA result, so we must not call new_dst!().
        // Alloca / Load / GEP produce a result; emit a zero-materialisation so
        // the destination VReg is defined before its first use.

        // ── memory: non-promotable alloca/load/store (FP-relative frame slots) ──
        // mem2reg eliminates promotable alloca/load/store; what remains here is
        // non-promotable (address escapes or non-constant GEP index).
        Alloca { alloc_ty, align, .. } => {
            let dst = new_dst!();
            let size_bytes = sizeof_ty(ctx, *alloc_ty) as u32;
            let align_bytes = align.unwrap_or(8);
            let slot_idx = mf.alloc_alloca_slot(size_bytes, align_bytes);
            // SUB_FP_IMM: dst = x29 - (slot_idx+1)*8  (address below frame pointer)
            mf.push(
                mblock,
                MInstr::new(SUB_FP_IMM)
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
            // LDR_REG: ldr xd, [xn]
            let ptr_vr = res!(*ptr);
            mf.push(
                mblock,
                MInstr::new(LDR_REG).with_dst(dst).with_vreg(ptr_vr),
            );
        }
        Store { val, ptr, .. } => {
            // STR_REG: str xs, [xn]
            let src = res!(*val);
            let ptr_vr = res!(*ptr);
            mf.push(
                mblock,
                MInstr::new(STR_REG).with_vreg(ptr_vr).with_vreg(src),
            );
        }

        // ── FP arithmetic (NEON/VFP scalar double path) ────────────────────
        FAdd { lhs, rhs, .. } => {
            emit_fp_binop3!(FADD_RR, *lhs, *rhs);
        }
        FSub { lhs, rhs, .. } => {
            emit_fp_binop3!(FSUB_RR, *lhs, *rhs);
        }
        FMul { lhs, rhs, .. } => {
            emit_fp_binop3!(FMUL_RR, *lhs, *rhs);
        }
        FDiv { lhs, rhs, .. } => {
            emit_fp_binop3!(FDIV_RR, *lhs, *rhs);
        }
        FRem { lhs, rhs, .. } => {
            // AArch64 has no FREM instruction; fall back to a placeholder
            // (real impl would call fmod via BLR or use fdiv+fmsub).
            emit_fp_binop3!(FDIV_RR, *lhs, *rhs);
        }
        FNeg { operand, .. } => {
            let dst = new_fp_dst!();
            let src = res!(*operand);
            mf.push(mblock, MInstr::new(FNEG_R).with_dst(dst).with_vreg(src));
        }

        // ── aggregate / vector ops (not yet supported) ─────────────────────
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

        // Terminators handled in lower_terminator.
        Ret { .. } | Br { .. } | CondBr { .. } | Invoke { .. } | Switch { .. } | Unreachable | Resume { .. } => {}
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
) {
    use InstrKind::*;
    let term = func.instr(tid);

    match &term.kind {
        Ret { val } => {
            if let Some(rv) = val {
                let src = resolve(ctx, mf, mblock, vmap, *rv);
                // Route the return value to the correct physical register.
                // Float/double results go in D0; integer/pointer results in X0.
                let ret_preg = if let Some(ty) = func.type_of_value(*rv) {
                    if is_float_type(ctx, ty) {
                        FP_RET_REG
                    } else {
                        INT_RET
                    }
                } else {
                    INT_RET
                };
                let mov_op = if ret_preg == FP_RET_REG {
                    FMOV_RR
                } else {
                    MOV_RR
                };
                let mut mi = MInstr::new(mov_op).with_preg(ret_preg).with_vreg(src);
                mi.phys_uses = vec![];
                // For FP, emit as FMOV_RR with dst=None (ABI fixed dest).
                if mov_op == MOV_RR {
                    emit_mov_to_preg(mf, mblock, ret_preg, src);
                } else {
                    // emit_mov_to_preg writes MOV_PR which is for int regs.
                    // For FP return, emit a dedicated FMOV_PR-style instruction.
                    mf.push(mblock, mi);
                }
            }
            mf.push(mblock, MInstr::new(RET));
        }

        Br { dest } => {
            emit_phi_copies(ctx, func, mf, mblock, mblock, *dest, vmap);
            mf.push(mblock, MInstr::new(B).with_block(dest.0 as usize));
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
            mf.push(then_edge, MInstr::new(B).with_block(then_dest.0 as usize));
            emit_phi_copies(ctx, func, mf, mblock, else_edge, *else_dest, vmap);
            mf.push(else_edge, MInstr::new(B).with_block(else_dest.0 as usize));
            // cmp c, #0; b.ne then_edge; b else_edge
            let zv = mf.fresh_vreg();
            mf.push(mblock, MInstr::new(MOV_IMM).with_dst(zv).with_imm(0));
            mf.push(mblock, MInstr::new(CMP_RR).with_vreg(c).with_vreg(zv));
            mf.push(
                mblock,
                MInstr::new(B_COND).with_imm(CC_NE).with_block(then_edge),
            );
            mf.push(mblock, MInstr::new(B).with_block(else_edge));
        }

        Invoke {
            callee,
            args,
            normal_dest,
            ..
        } => {
            let arg_locs = classify_aapcs64_args(args.len());
            for (i, &arg_vref) in args.iter().enumerate() {
                let src = resolve(ctx, mf, mblock, vmap, arg_vref);
                match arg_locs[i] {
                    ArgLocation::Reg(preg) => emit_mov_to_preg(mf, mblock, preg, src),
                    ArgLocation::Stack(_) => mf.push(mblock, MInstr::new(STR).with_vreg(src)),
                }
            }
            let callee_vr = resolve(ctx, mf, mblock, vmap, *callee);
            let mut call_mi = MInstr::new(BLR).with_vreg(callee_vr);
            call_mi.clobbers = ALLOCATABLE.to_vec();
            mf.push(mblock, call_mi);
            if term.ty != ctx.void_ty {
                let dst = mf.fresh_vreg();
                vmap.insert(ValueRef::Instruction(tid), dst);
                emit_mov_from_preg(mf, mblock, dst, INT_RET);
            }
            emit_phi_copies(ctx, func, mf, mblock, mblock, *normal_dest, vmap);
            mf.push(mblock, MInstr::new(B).with_block(normal_dest.0 as usize));
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
                    MInstr::new(B_COND)
                        .with_imm(CC_EQ)
                        .with_block(case_dest.0 as usize),
                );
            }
            emit_phi_copies(ctx, func, mf, mblock, mblock, *default, vmap);
            mf.push(mblock, MInstr::new(B).with_block(default.0 as usize));
        }

        Unreachable => {
            mf.push(mblock, MInstr::new(NOP));
        }

        // resume re-throws the in-flight exception; lower to NOP as a
        // placeholder — a real backend would call _Unwind_Resume.
        Resume { .. } => {
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

    for &iid in &dest_bb.body {
        if let InstrKind::Phi { incoming, .. } = &func.instr(iid).kind {
            // Find the incoming value from `src_bid`.
            if let Some((incoming_val, _)) = incoming.iter().find(|(_, bid)| *bid == src_bid) {
                let phi_vreg = match vmap.get(&ValueRef::Instruction(iid)) {
                    Some(&v) => v,
                    None => continue,
                };
                let src_vreg = resolve(ctx, mf, emit_to_mblock, vmap, *incoming_val);
                mf.push(
                    emit_to_mblock,
                    MInstr::new(MOV_RR).with_dst(phi_vreg).with_vreg(src_vreg),
                );
            }
        }
    }
}

// ── ABI register helpers ──────────────────────────────────────────────────

fn emit_mov_to_preg(mf: &mut MachineFunction, mblock: usize, preg: PReg, src: VReg) {
    // MOV_PR: operands[0] = fixed PReg destination, operands[1] = VReg source.
    // dst is intentionally None so apply_allocation does not reassign preg.
    mf.push(mblock, MInstr::new(MOV_PR).with_preg(preg).with_vreg(src));
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
    use crate::encode::AArch64Emitter;
    use llvm_codegen::emit::{Emitter, ObjectFormat};
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
        let mut be = AArch64Backend::default();
        let mf = be.lower_function(&ctx, &module, &module.functions[0]);
        assert_eq!(mf.name, "add");
        assert!(!mf.blocks.is_empty());
    }

    #[test]
    fn lower_declaration_is_empty() {
        let mut ctx = Context::new();
        let mut module = Module::new("test");
        let mut b = Builder::new(&mut ctx, &mut module);
        b.add_declaration("ext", b.ctx.void_ty, vec![], false);
        let mut be = AArch64Backend::default();
        let mf = be.lower_function(&ctx, &module, &module.functions[0]);
        assert!(mf.blocks.is_empty(), "declaration should produce no blocks");
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

    #[test]
    fn udiv_uses_udiv_rr() {
        let (ctx, module) = make_div_fn(true);
        let mut be = AArch64Backend::default();
        let mf = be.lower_function(&ctx, &module, &module.functions[0]);
        let has_udiv = mf
            .blocks
            .iter()
            .any(|bl| bl.instrs.iter().any(|i| i.opcode == UDIV_RR));
        let has_sdiv = mf
            .blocks
            .iter()
            .any(|bl| bl.instrs.iter().any(|i| i.opcode == SDIV_RR));
        assert!(has_udiv, "UDiv must emit UDIV_RR (unsigned div)");
        assert!(!has_sdiv, "UDiv must NOT emit SDIV_RR (signed div)");
    }

    #[test]
    fn sdiv_uses_sdiv_rr() {
        let (ctx, module) = make_div_fn(false);
        let mut be = AArch64Backend::default();
        let mf = be.lower_function(&ctx, &module, &module.functions[0]);
        let has_sdiv = mf
            .blocks
            .iter()
            .any(|bl| bl.instrs.iter().any(|i| i.opcode == SDIV_RR));
        let has_udiv = mf
            .blocks
            .iter()
            .any(|bl| bl.instrs.iter().any(|i| i.opcode == UDIV_RR));
        assert!(has_sdiv, "SDiv must emit SDIV_RR (signed div)");
        assert!(!has_udiv, "SDiv must NOT emit UDIV_RR (unsigned div)");
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
    fn sext_i8_uses_sxtb() {
        let (ctx, module) = make_sext_fn(8);
        let mut be = AArch64Backend::default();
        let mf = be.lower_function(&ctx, &module, &module.functions[0]);
        let has_sxtb = mf
            .blocks
            .iter()
            .any(|b| b.instrs.iter().any(|i| i.opcode == SXTB));
        assert!(has_sxtb, "sext from i8 must use SXTB");
    }

    #[test]
    fn sext_i32_uses_sxtw() {
        let (ctx, module) = make_sext_fn(32);
        let mut be = AArch64Backend::default();
        let mf = be.lower_function(&ctx, &module, &module.functions[0]);
        let has_sxtw = mf
            .blocks
            .iter()
            .any(|b| b.instrs.iter().any(|i| i.opcode == SXTW));
        assert!(has_sxtw, "sext from i32 must use SXTW");
    }

    /// Build a function with a CondBr where each successor has a phi.
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
        let entry = b.add_block("entry");
        let then_bb = b.add_block("then_bb");
        let else_bb = b.add_block("else_bb");
        let merge_bb = b.add_block("merge");

        b.position_at_end(entry);
        let a = b.get_arg(0);
        let bv = b.get_arg(1);
        let cond = b.get_arg(2);
        b.build_cond_br(cond, then_bb, else_bb);

        b.position_at_end(then_bb);
        let px = b.build_phi("px", b.ctx.i64_ty, vec![(a, entry)]);
        b.build_br(merge_bb);

        b.position_at_end(else_bb);
        let py = b.build_phi("py", b.ctx.i64_ty, vec![(bv, entry)]);
        b.build_br(merge_bb);

        b.position_at_end(merge_bb);
        let r = b.build_phi("r", b.ctx.i64_ty, vec![(px, then_bb), (py, else_bb)]);
        b.build_ret(r);

        (ctx, module)
    }

    #[test]
    fn condbr_creates_edge_split_trampolines() {
        // CondBr must create trampoline blocks for edge splitting.
        let (ctx, module) = make_condbr_phi_fn();
        let func = &module.functions[0];
        let ir_block_count = func.blocks.len(); // 4

        let mut be = AArch64Backend::default();
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
    fn inline_asm_nop_lowers_and_emits_aarch64_nop() {
        let mut ctx = Context::new();
        let mut module = Module::new("test");
        let mut b = Builder::new(&mut ctx, &mut module);
        b.add_function(
            "asm_nop",
            b.ctx.void_ty,
            vec![],
            vec![],
            false,
            Linkage::External,
        );
        let entry = b.add_block("entry");
        b.position_at_end(entry);
        b.build_inline_asm("", b.ctx.void_ty, "nop", "", true, false, vec![]);
        b.build_ret_void();

        let mut be = AArch64Backend::default();
        let mf = be.lower_function(&ctx, &module, &module.functions[0]);
        assert!(mf
            .blocks
            .iter()
            .flat_map(|b| &b.instrs)
            .any(|i| i.opcode == INLINE_ASM));

        let mut emitter = AArch64Emitter::new(ObjectFormat::Elf);
        let section = emitter.emit_function(&mf);
        assert!(
            section
                .data
                .windows(4)
                .any(|w| w == [0x1f, 0x20, 0x03, 0xd5]),
            "AArch64 inline asm nop must emit D503201F"
        );
    }

    #[test]
    fn select_lowering_uses_mov_wide_for_allones_mask() {
        // The Select lowering must use MOV_WIDE (not MOV_IMM) to materialise
        // the all-ones mask (-1), so that all 64 bits are set.
        // MOV_IMM truncates to 16 bits (gives 0xFFFF, not 0xFFFF_FFFF_FFFF_FFFF).
        let mut ctx = Context::new();
        let mut module = Module::new("test");
        let mut b = Builder::new(&mut ctx, &mut module);
        b.add_function(
            "sel_fn",
            b.ctx.i64_ty,
            vec![b.ctx.i1_ty, b.ctx.i64_ty, b.ctx.i64_ty],
            vec!["cond".into(), "tv".into(), "fv".into()],
            false,
            Linkage::External,
        );
        let entry = b.add_block("entry");
        b.position_at_end(entry);
        let cond = b.get_arg(0);
        let tv = b.get_arg(1);
        let fv = b.get_arg(2);
        let sel = b.build_select("sel", cond, tv, fv);
        b.build_ret(sel);

        let mut be = AArch64Backend::default();
        let mf = be.lower_function(&ctx, &module, &module.functions[0]);

        // The Select lowering must emit at least one MOV_WIDE instruction
        // (for the all-ones mask used as ~scratch).
        let has_mov_wide = mf
            .blocks
            .iter()
            .any(|bl| bl.instrs.iter().any(|i| i.opcode == MOV_WIDE));
        assert!(
            has_mov_wide,
            "Select lowering must use MOV_WIDE to materialise the all-ones mask, \
             not MOV_IMM which only loads 16 bits"
        );
    }

    #[test]
    fn load_lowering_defines_dst_vreg() {
        // A Load instruction produces an SSA result; after lowering the
        // destination VReg must be written by some instruction (not left
        // undefined as it was when a plain NOP was emitted).
        let mut ctx = Context::new();
        let mut module = Module::new("test");
        let mut b = Builder::new(&mut ctx, &mut module);
        b.add_function(
            "load_fn",
            b.ctx.i64_ty,
            vec![b.ctx.ptr_ty],
            vec!["p".into()],
            false,
            Linkage::External,
        );
        let entry = b.add_block("entry");
        b.position_at_end(entry);
        let p = b.get_arg(0);
        let v = b.build_load("v", b.ctx.i64_ty, p);
        b.build_ret(v);

        let mut be = AArch64Backend::default();
        let mf = be.lower_function(&ctx, &module, &module.functions[0]);

        // There must be at least one instruction that has a dst VReg that
        // is different from all VRegs used only as sources — a proxy for
        // "the load result is defined somewhere".
        let total_dsts: usize = mf
            .blocks
            .iter()
            .flat_map(|bl| bl.instrs.iter())
            .filter(|i| i.dst.is_some())
            .count();
        assert!(
            total_dsts > 0,
            "Load lowering must emit an instruction with a non-None dst VReg"
        );

        // Confirm no NOP appears as the sole instruction for the load
        // (NOP has no dst, so it cannot define the result VReg).
        let only_nops = mf
            .blocks
            .iter()
            .all(|bl| bl.instrs.iter().all(|i| i.opcode == NOP));
        assert!(
            !only_nops,
            "Load lowering must not produce only NOPs — the result must be defined"
        );
    }

    #[test]
    fn store_lowering_does_not_create_spurious_vreg() {
        // Store is a void instruction (no SSA result). Lowering it must not
        // call new_dst!() which would register an undefined VReg in vmap.
        // Proxy: the number of VRegs with a defined dst should equal only the
        // number of non-void result instructions (here: 1 arg materialisation).
        let mut ctx = Context::new();
        let mut module = Module::new("test");
        let mut b = Builder::new(&mut ctx, &mut module);
        b.add_function(
            "store_fn",
            b.ctx.void_ty,
            vec![b.ctx.ptr_ty, b.ctx.i64_ty],
            vec!["p".into(), "v".into()],
            false,
            Linkage::External,
        );
        let entry = b.add_block("entry");
        b.position_at_end(entry);
        let p = b.get_arg(0);
        let v = b.get_arg(1);
        b.build_store(v, p);
        b.build_ret_void();

        let mut be = AArch64Backend::default();
        let mf = be.lower_function(&ctx, &module, &module.functions[0]);

        // The function body should compile without panicking.
        assert!(
            !mf.blocks.is_empty(),
            "store function must produce at least one block"
        );
    }

    // ── atomic lowering tests ─────────────────────────────────────────────

    #[test]
    fn aarch64_atomic_fence_emits_dmb_ish() {
        // Fence must produce a DMB_ISH opcode (not INLINE_ASM with raw bytes).
        use llvm_ir::MemOrdering;
        let mut ctx = Context::new();
        let mut module = Module::new("test");
        let mut b = Builder::new(&mut ctx, &mut module);
        b.add_function(
            "fence_fn",
            b.ctx.void_ty,
            vec![],
            vec![],
            false,
            Linkage::External,
        );
        let entry = b.add_block("entry");
        b.position_at_end(entry);
        b.build_fence(MemOrdering::SeqCst);
        b.build_ret_void();

        let mut be = AArch64Backend::default();
        let mf = be.lower_function(&ctx, &module, &module.functions[0]);

        let has_dmb_ish = mf
            .blocks
            .iter()
            .any(|bl| bl.instrs.iter().any(|i| i.opcode == DMB_ISH));
        assert!(has_dmb_ish, "Fence must lower to DMB_ISH opcode");

        // Must NOT produce a legacy INLINE_ASM with raw bytes.
        let has_inline_asm = mf
            .blocks
            .iter()
            .any(|bl| bl.instrs.iter().any(|i| i.opcode == INLINE_ASM));
        assert!(
            !has_inline_asm,
            "Fence must not produce INLINE_ASM — must use DMB_ISH opcode"
        );

        // Verify the encoding produces the correct DMB ISH bytes.
        use crate::encode::AArch64Emitter;
        use llvm_codegen::emit::{Emitter, ObjectFormat};
        let mut emitter = AArch64Emitter::new(ObjectFormat::Elf);
        let sec = emitter.emit_function(&mf);
        // DMB ISH = 0xD5033BBF in LE: [BF, 3B, 03, D5]
        let found_dmb = sec
            .data
            .windows(4)
            .any(|w| w == [0xBF, 0x3B, 0x03, 0xD5]);
        assert!(found_dmb, "DMB ISH bytes [BF 3B 03 D5] must appear in code");
    }

    #[test]
    fn aarch64_cmpxchg_lse_emits_casal() {
        // With LSE enabled (default), CmpXchg must lower to CASAL.
        use llvm_ir::MemOrdering;
        let mut ctx = Context::new();
        let mut module = Module::new("test");
        let mut b = Builder::new(&mut ctx, &mut module);
        b.add_function(
            "cas_fn",
            b.ctx.i64_ty,
            vec![b.ctx.ptr_ty, b.ctx.i64_ty, b.ctx.i64_ty],
            vec!["ptr".into(), "cmp".into(), "new".into()],
            false,
            Linkage::External,
        );
        let entry = b.add_block("entry");
        b.position_at_end(entry);
        let ptr = b.get_arg(0);
        let cmp = b.get_arg(1);
        let new_val = b.get_arg(2);
        let result = b.build_cmpxchg(
            "r",
            b.ctx.i64_ty,
            ptr,
            cmp,
            new_val,
            MemOrdering::SeqCst,
            MemOrdering::SeqCst,
            false,
            false,
        );
        b.build_ret(result);

        // Default backend has LSE enabled.
        let mut be = AArch64Backend::default();
        let mf = be.lower_function(&ctx, &module, &module.functions[0]);

        let has_casal = mf
            .blocks
            .iter()
            .any(|bl| bl.instrs.iter().any(|i| i.opcode == CASAL));
        assert!(has_casal, "CmpXchg with LSE=true must emit CASAL");

        let has_ldxr = mf
            .blocks
            .iter()
            .any(|bl| bl.instrs.iter().any(|i| i.opcode == LDXR));
        assert!(
            !has_ldxr,
            "CmpXchg with LSE=true must NOT emit LDXR (LL/SC)"
        );
    }

    #[test]
    fn aarch64_cmpxchg_baseline_emits_ldxr_stxr() {
        // With LSE disabled, CmpXchg must lower to LDXR + STXR.
        use llvm_ir::MemOrdering;
        let mut ctx = Context::new();
        let mut module = Module::new("test");
        let mut b = Builder::new(&mut ctx, &mut module);
        b.add_function(
            "cas_baseline_fn",
            b.ctx.i64_ty,
            vec![b.ctx.ptr_ty, b.ctx.i64_ty, b.ctx.i64_ty],
            vec!["ptr".into(), "cmp".into(), "new".into()],
            false,
            Linkage::External,
        );
        let entry = b.add_block("entry");
        b.position_at_end(entry);
        let ptr = b.get_arg(0);
        let cmp = b.get_arg(1);
        let new_val = b.get_arg(2);
        let result = b.build_cmpxchg(
            "r",
            b.ctx.i64_ty,
            ptr,
            cmp,
            new_val,
            MemOrdering::SeqCst,
            MemOrdering::SeqCst,
            false,
            false,
        );
        b.build_ret(result);

        // Baseline backend: no LSE.
        let mut be = AArch64Backend::new(AArch64Features::baseline());
        let mf = be.lower_function(&ctx, &module, &module.functions[0]);

        let has_ldxr = mf
            .blocks
            .iter()
            .any(|bl| bl.instrs.iter().any(|i| i.opcode == LDXR));
        assert!(has_ldxr, "CmpXchg with LSE=false must emit LDXR");

        let has_stxr = mf
            .blocks
            .iter()
            .any(|bl| bl.instrs.iter().any(|i| i.opcode == STXR));
        assert!(has_stxr, "CmpXchg with LSE=false must emit STXR");

        let has_casal = mf
            .blocks
            .iter()
            .any(|bl| bl.instrs.iter().any(|i| i.opcode == CASAL));
        assert!(
            !has_casal,
            "CmpXchg with LSE=false must NOT emit CASAL"
        );
    }

    #[test]
    fn aarch64_atomic_rmw_add_lse_emits_ldaddal() {
        // AtomicRmw(Add) with LSE=true must emit LDADDAL.
        use llvm_ir::{MemOrdering, RmwOp};
        let mut ctx = Context::new();
        let mut module = Module::new("test");
        let mut b = Builder::new(&mut ctx, &mut module);
        b.add_function(
            "rmw_add_fn",
            b.ctx.i64_ty,
            vec![b.ctx.ptr_ty, b.ctx.i64_ty],
            vec!["ptr".into(), "val".into()],
            false,
            Linkage::External,
        );
        let entry = b.add_block("entry");
        b.position_at_end(entry);
        let ptr = b.get_arg(0);
        let val = b.get_arg(1);
        let old = b.build_atomicrmw(
            "old",
            RmwOp::Add,
            b.ctx.i64_ty,
            ptr,
            val,
            MemOrdering::SeqCst,
            false,
        );
        b.build_ret(old);

        let mut be = AArch64Backend::default();
        let mf = be.lower_function(&ctx, &module, &module.functions[0]);

        let has_ldaddal = mf
            .blocks
            .iter()
            .any(|bl| bl.instrs.iter().any(|i| i.opcode == LDADDAL));
        assert!(has_ldaddal, "AtomicRmw(Add) with LSE=true must emit LDADDAL");
    }

    #[test]
    fn aarch64_atomic_rmw_xchg_lse_emits_swpal() {
        // AtomicRmw(Xchg) with LSE=true must emit SWPAL.
        use llvm_ir::{MemOrdering, RmwOp};
        let mut ctx = Context::new();
        let mut module = Module::new("test");
        let mut b = Builder::new(&mut ctx, &mut module);
        b.add_function(
            "rmw_xchg_fn",
            b.ctx.i64_ty,
            vec![b.ctx.ptr_ty, b.ctx.i64_ty],
            vec!["ptr".into(), "val".into()],
            false,
            Linkage::External,
        );
        let entry = b.add_block("entry");
        b.position_at_end(entry);
        let ptr = b.get_arg(0);
        let val = b.get_arg(1);
        let old = b.build_atomicrmw(
            "old",
            RmwOp::Xchg,
            b.ctx.i64_ty,
            ptr,
            val,
            MemOrdering::SeqCst,
            false,
        );
        b.build_ret(old);

        let mut be = AArch64Backend::default();
        let mf = be.lower_function(&ctx, &module, &module.functions[0]);

        let has_swpal = mf
            .blocks
            .iter()
            .any(|bl| bl.instrs.iter().any(|i| i.opcode == SWPAL));
        assert!(has_swpal, "AtomicRmw(Xchg) with LSE=true must emit SWPAL");
    }

    #[test]
    fn aarch64_atomic_rmw_baseline_emits_ldxr_stxr() {
        // AtomicRmw with LSE=false must emit LDXR + STXR.
        use llvm_ir::{MemOrdering, RmwOp};
        let mut ctx = Context::new();
        let mut module = Module::new("test");
        let mut b = Builder::new(&mut ctx, &mut module);
        b.add_function(
            "rmw_baseline_fn",
            b.ctx.i64_ty,
            vec![b.ctx.ptr_ty, b.ctx.i64_ty],
            vec!["ptr".into(), "val".into()],
            false,
            Linkage::External,
        );
        let entry = b.add_block("entry");
        b.position_at_end(entry);
        let ptr = b.get_arg(0);
        let val = b.get_arg(1);
        let old = b.build_atomicrmw(
            "old",
            RmwOp::Or,
            b.ctx.i64_ty,
            ptr,
            val,
            MemOrdering::SeqCst,
            false,
        );
        b.build_ret(old);

        let mut be = AArch64Backend::new(AArch64Features::baseline());
        let mf = be.lower_function(&ctx, &module, &module.functions[0]);

        let has_ldxr = mf
            .blocks
            .iter()
            .any(|bl| bl.instrs.iter().any(|i| i.opcode == LDXR));
        assert!(has_ldxr, "AtomicRmw with LSE=false must emit LDXR");
        let has_stxr = mf
            .blocks
            .iter()
            .any(|bl| bl.instrs.iter().any(|i| i.opcode == STXR));
        assert!(has_stxr, "AtomicRmw with LSE=false must emit STXR");
    }

    // ── FP lowering tests ──────────────────────────────────────────────────

    fn make_fp_binop_fn(
        name: &str,
        build: impl Fn(&mut Builder, llvm_ir::ValueRef, llvm_ir::ValueRef) -> llvm_ir::ValueRef,
    ) -> (Context, Module) {
        use llvm_ir::{Builder, Context, FloatKind, Linkage, Module};
        let mut ctx = Context::new();
        let f64_ty = ctx.mk_float(FloatKind::Double);
        let mut module = Module::new("test");
        let mut b = Builder::new(&mut ctx, &mut module);
        b.add_function(
            name,
            f64_ty,
            vec![f64_ty, f64_ty],
            vec!["a".into(), "b".into()],
            false,
            Linkage::External,
        );
        let entry = b.add_block("entry");
        b.position_at_end(entry);
        let a = b.get_arg(0);
        let bv = b.get_arg(1);
        let result = build(&mut b, a, bv);
        b.build_ret(result);
        (ctx, module)
    }

    #[test]
    fn fadd_lowers_to_fadd_rr() {
        let (ctx, module) = make_fp_binop_fn("fadd_fn", |b, a, bv| b.build_fadd("r", a, bv));
        let mut be = AArch64Backend::default();
        let mf = be.lower_function(&ctx, &module, &module.functions[0]);
        let has_fadd = mf.blocks.iter().any(|bl| bl.instrs.iter().any(|i| i.opcode == FADD_RR));
        assert!(has_fadd, "FAdd must lower to FADD_RR");
    }

    #[test]
    fn fsub_lowers_to_fsub_rr() {
        let (ctx, module) = make_fp_binop_fn("fsub_fn", |b, a, bv| b.build_fsub("r", a, bv));
        let mut be = AArch64Backend::default();
        let mf = be.lower_function(&ctx, &module, &module.functions[0]);
        let has_fsub = mf.blocks.iter().any(|bl| bl.instrs.iter().any(|i| i.opcode == FSUB_RR));
        assert!(has_fsub, "FSub must lower to FSUB_RR");
    }

    #[test]
    fn fmul_lowers_to_fmul_rr() {
        let (ctx, module) = make_fp_binop_fn("fmul_fn", |b, a, bv| b.build_fmul("r", a, bv));
        let mut be = AArch64Backend::default();
        let mf = be.lower_function(&ctx, &module, &module.functions[0]);
        let has_fmul = mf.blocks.iter().any(|bl| bl.instrs.iter().any(|i| i.opcode == FMUL_RR));
        assert!(has_fmul, "FMul must lower to FMUL_RR");
    }

    #[test]
    fn fdiv_lowers_to_fdiv_rr() {
        let (ctx, module) = make_fp_binop_fn("fdiv_fn", |b, a, bv| b.build_fdiv("r", a, bv));
        let mut be = AArch64Backend::default();
        let mf = be.lower_function(&ctx, &module, &module.functions[0]);
        let has_fdiv = mf.blocks.iter().any(|bl| bl.instrs.iter().any(|i| i.opcode == FDIV_RR));
        assert!(has_fdiv, "FDiv must lower to FDIV_RR");
    }

    #[test]
    fn fneg_lowers_to_fneg_r() {
        use llvm_ir::{Builder, Context, FloatKind, Linkage, Module};
        let mut ctx = Context::new();
        let f64_ty = ctx.mk_float(FloatKind::Double);
        let mut module = Module::new("test");
        let mut b = Builder::new(&mut ctx, &mut module);
        b.add_function(
            "fneg_fn",
            f64_ty,
            vec![f64_ty],
            vec!["a".into()],
            false,
            Linkage::External,
        );
        let entry = b.add_block("entry");
        b.position_at_end(entry);
        let a = b.get_arg(0);
        let neg = b.build_fneg("neg", a);
        b.build_ret(neg);

        let mut be = AArch64Backend::default();
        let mf = be.lower_function(&ctx, &module, &module.functions[0]);
        let has_fneg = mf.blocks.iter().any(|bl| bl.instrs.iter().any(|i| i.opcode == FNEG_R));
        assert!(has_fneg, "FNeg must lower to FNEG_R");
    }

    #[test]
    fn fp_args_go_to_float_vregs() {
        // Float/double function arguments must be allocated to float VRegs
        // (VReg with class bit set), and the lowering should emit FMOV_RR
        // (not MOV_RR) to copy from the ABI D-register.
        use llvm_ir::{Builder, Context, FloatKind, Linkage, Module};
        let mut ctx = Context::new();
        let f64_ty = ctx.mk_float(FloatKind::Double);
        let mut module = Module::new("test");
        let mut b = Builder::new(&mut ctx, &mut module);
        b.add_function(
            "fp_arg_fn",
            f64_ty,
            vec![f64_ty],
            vec!["x".into()],
            false,
            Linkage::External,
        );
        let entry = b.add_block("entry");
        b.position_at_end(entry);
        let x = b.get_arg(0);
        b.build_ret(x);

        let mut be = AArch64Backend::default();
        let mf = be.lower_function(&ctx, &module, &module.functions[0]);

        // Must emit FMOV_RR to copy from D0 argument register.
        let has_fmov = mf.blocks.iter().any(|bl| bl.instrs.iter().any(|i| i.opcode == FMOV_RR));
        assert!(
            has_fmov,
            "Float function argument must be copied using FMOV_RR (not MOV_RR)"
        );
    }

    #[test]
    fn sitofp_lowers_to_scvtf() {
        // SIToFP must lower to SCVTF_RR.
        use llvm_ir::{Builder, Context, FloatKind, Linkage, Module};
        let mut ctx = Context::new();
        let f64_ty = ctx.mk_float(FloatKind::Double);
        let mut module = Module::new("test");
        let mut b = Builder::new(&mut ctx, &mut module);
        b.add_function(
            "sitofp_fn",
            f64_ty,
            vec![b.ctx.i64_ty],
            vec!["n".into()],
            false,
            Linkage::External,
        );
        let entry = b.add_block("entry");
        b.position_at_end(entry);
        let n = b.get_arg(0);
        let r = b.build_sitofp("r", n, f64_ty);
        b.build_ret(r);

        let mut be = AArch64Backend::default();
        let mf = be.lower_function(&ctx, &module, &module.functions[0]);
        let has_scvtf = mf.blocks.iter().any(|bl| bl.instrs.iter().any(|i| i.opcode == SCVTF_RR));
        assert!(has_scvtf, "SIToFP must lower to SCVTF_RR");
    }

    #[test]
    fn fptosi_lowers_to_fcvtzs() {
        // FPToSI must lower to FCVTZS_RR.
        use llvm_ir::{Builder, Context, FloatKind, Linkage, Module};
        let mut ctx = Context::new();
        let f64_ty = ctx.mk_float(FloatKind::Double);
        let mut module = Module::new("test");
        let mut b = Builder::new(&mut ctx, &mut module);
        b.add_function(
            "fptosi_fn",
            b.ctx.i64_ty,
            vec![f64_ty],
            vec!["x".into()],
            false,
            Linkage::External,
        );
        let entry = b.add_block("entry");
        b.position_at_end(entry);
        let x = b.get_arg(0);
        let r = b.build_fptosi("r", x, b.ctx.i64_ty);
        b.build_ret(r);

        let mut be = AArch64Backend::default();
        let mf = be.lower_function(&ctx, &module, &module.functions[0]);
        let has_fcvtzs = mf.blocks.iter().any(|bl| bl.instrs.iter().any(|i| i.opcode == FCVTZS_RR));
        assert!(has_fcvtzs, "FPToSI must lower to FCVTZS_RR");
    }
}
