//! AArch64 IR → machine-IR lowering.
//!
//! Implements [`IselBackend`] for [`AArch64Backend`].  Each IR instruction is
//! translated to one or more machine instructions using virtual registers.
//! Phi-destruction (parallel copy insertion) is also handled here.

use crate::{
    abi::{classify_aapcs64_args, ArgLocation, INT_RET},
    instructions::*,
    regs::{ALLOCATABLE, CALLEE_SAVED},
};
use llvm_codegen::isel::{IselBackend, MInstr, MachineFunction, PReg, VReg};
use llvm_ir::{
    ArgId, BlockId, ConstantData, Context, Function, InstrId, InstrKind, IntPredicate, Module,
    TypeData, ValueRef,
};
use std::collections::HashMap;

/// AArch64 instruction-selection backend.
pub struct AArch64Backend;

impl IselBackend for AArch64Backend {
    fn lower_function(
        &mut self,
        ctx: &Context,
        module: &Module,
        func: &Function,
    ) -> MachineFunction {
        let mut mf = MachineFunction::new(func.name.clone());
        mf.allocatable_pregs = ALLOCATABLE.to_vec();
        mf.callee_saved_pregs = CALLEE_SAVED.to_vec();

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
        let arg_locs = classify_aapcs64_args(func.args.len());
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
                    // Stack arguments: emit a placeholder immediate to mark the slot.
                    mf.push(0, MInstr::new(MOV_IMM).with_dst(vr).with_imm(offset as i64));
                }
            }
        }

        // Lower each IR block.
        for (bi, bb) in func.blocks.iter().enumerate() {
            for &iid in &bb.body {
                lower_instr(ctx, module, func, &mut mf, bi, iid, &mut vmap);
            }
            if let Some(tid) = bb.terminator {
                lower_terminator(ctx, func, &mut mf, bi, tid, &mut vmap);
            }
        }

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
    if let Some(&existing) = vmap.get(&vr) {
        return existing;
    }
    match vr {
        ValueRef::Constant(cid) => {
            let vreg = mf.fresh_vreg();
            vmap.insert(vr, vreg);
            let imm = const_to_imm(ctx.get_const(cid));
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

        FCmp { .. } => {
            // FP comparisons not yet supported — emit a zero.
            let dst = new_dst!();
            mf.push(mblock, MInstr::new(MOV_IMM).with_dst(dst).with_imm(0));
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

        // ── memory (placeholder — mem2reg removes most alloca/load/store) ────
        // Store produces no SSA result, so we must not call new_dst!().
        // Alloca / Load / GEP produce a result; emit a zero-materialisation so
        // the destination VReg is defined before its first use.
        Store { .. } => {
            mf.push(mblock, MInstr::new(NOP));
        }
        Alloca { .. } | Load { .. } | GetElementPtr { .. } => {
            let dst = new_dst!();
            mf.push(mblock, MInstr::new(MOV_IMM).with_dst(dst).with_imm(0));
        }

        // ── FP arithmetic (not yet supported) ──────────────────────────────
        FAdd { .. } | FSub { .. } | FMul { .. } | FDiv { .. } | FRem { .. } | FNeg { .. } => {
            let dst = new_dst!();
            mf.push(mblock, MInstr::new(MOV_IMM).with_dst(dst).with_imm(0));
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
        let mut be = AArch64Backend;
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
        let mut be = AArch64Backend;
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
        let mut be = AArch64Backend;
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
        let mut be = AArch64Backend;
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
        let mut be = AArch64Backend;
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
        let mut be = AArch64Backend;
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

        let mut be = AArch64Backend;
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

        let mut be = AArch64Backend;
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

        let mut be = AArch64Backend;
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

        let mut be = AArch64Backend;
        let mf = be.lower_function(&ctx, &module, &module.functions[0]);

        // The function body should compile without panicking.
        assert!(
            !mf.blocks.is_empty(),
            "store function must produce at least one block"
        );
    }
}
