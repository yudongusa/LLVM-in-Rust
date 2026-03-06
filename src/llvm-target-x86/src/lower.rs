//! x86_64 IR → machine-IR lowering.
//!
//! Implements [`IselBackend`] for [`X86Backend`].  Each IR instruction is
//! translated to one or more machine instructions using virtual registers.
//! Phi-destruction (parallel copy insertion) is also handled here.

use crate::{
    abi::{classify_sysv_args, ArgLocation, SYSV_INT_RET},
    instructions::*,
    regs::{ALLOCATABLE, CALLEE_SAVED, RCX, RDX},
};
use llvm_codegen::isel::{IselBackend, MInstr, MachineFunction, PReg, VReg};
use llvm_ir::{
    ArgId, BlockId, ConstantData, Context, Function, InstrId, InstrKind, IntPredicate, Module,
    TypeData, ValueRef,
};
use std::collections::HashMap;

/// x86_64 instruction-selection backend.
pub struct X86Backend;

impl IselBackend for X86Backend {
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
        let arg_locs = classify_sysv_args(func.args.len());
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
            mf.push(mblock, MInstr::new(MOV_RI).with_dst(vreg).with_imm(imm));
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
        IntPredicate::Ult => CC_ULT,
        IntPredicate::Ule => CC_ULE,
        IntPredicate::Ugt => CC_UGT,
        IntPredicate::Uge => CC_UGE,
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
            emit_binop!(ADD_RR, *lhs, *rhs);
        }
        Sub { lhs, rhs, .. } => {
            emit_binop!(SUB_RR, *lhs, *rhs);
        }
        Mul { lhs, rhs, .. } => {
            emit_binop!(IMUL_RR, *lhs, *rhs);
        }

        SDiv { lhs, rhs, .. } => {
            let dst = new_dst!();
            let l = res!(*lhs);
            let r = res!(*rhs);
            // mov rax, lhs; cqo; idiv rcx → rax = quotient (signed)
            // Keep divisor out of clobbered regs (rax/rdx) to avoid
            // self-clobbering when the allocator picks those registers.
            emit_mov_to_preg(mf, mblock, RCX, r);
            emit_mov_to_preg(mf, mblock, SYSV_INT_RET, l);
            mf.push(mblock, MInstr::new(CQO));
            let mut div_mi = MInstr::new(IDIV_R).with_preg(RCX);
            div_mi.phys_uses = vec![RCX];
            div_mi.clobbers = vec![SYSV_INT_RET, RDX];
            mf.push(mblock, div_mi);
            emit_mov_from_preg(mf, mblock, dst, SYSV_INT_RET);
        }

        UDiv { lhs, rhs, .. } => {
            let dst = new_dst!();
            let l = res!(*lhs);
            let r = res!(*rhs);
            // mov rax, lhs; xor rdx, rdx; div rcx → rax = quotient (unsigned)
            emit_mov_to_preg(mf, mblock, RCX, r);
            emit_mov_to_preg(mf, mblock, SYSV_INT_RET, l);
            let zero = mf.fresh_vreg();
            mf.push(mblock, MInstr::new(MOV_RI).with_dst(zero).with_imm(0));
            emit_mov_to_preg(mf, mblock, RDX, zero);
            let mut div_mi = MInstr::new(DIV_R).with_preg(RCX);
            div_mi.phys_uses = vec![RCX];
            div_mi.clobbers = vec![SYSV_INT_RET, RDX];
            mf.push(mblock, div_mi);
            emit_mov_from_preg(mf, mblock, dst, SYSV_INT_RET);
        }

        SRem { lhs, rhs, .. } => {
            let dst = new_dst!();
            let l = res!(*lhs);
            let r = res!(*rhs);
            // mov rax, lhs; cqo; idiv rcx → rdx = remainder (signed)
            emit_mov_to_preg(mf, mblock, RCX, r);
            emit_mov_to_preg(mf, mblock, SYSV_INT_RET, l);
            mf.push(mblock, MInstr::new(CQO));
            let mut div_mi = MInstr::new(IDIV_R).with_preg(RCX);
            div_mi.phys_uses = vec![RCX];
            div_mi.clobbers = vec![SYSV_INT_RET, RDX];
            mf.push(mblock, div_mi);
            emit_mov_from_preg(mf, mblock, dst, RDX);
        }

        URem { lhs, rhs, .. } => {
            let dst = new_dst!();
            let l = res!(*lhs);
            let r = res!(*rhs);
            // mov rax, lhs; xor rdx, rdx; div rcx → rdx = remainder (unsigned)
            emit_mov_to_preg(mf, mblock, RCX, r);
            emit_mov_to_preg(mf, mblock, SYSV_INT_RET, l);
            let zero = mf.fresh_vreg();
            mf.push(mblock, MInstr::new(MOV_RI).with_dst(zero).with_imm(0));
            emit_mov_to_preg(mf, mblock, RDX, zero);
            let mut div_mi = MInstr::new(DIV_R).with_preg(RCX);
            div_mi.phys_uses = vec![RCX];
            div_mi.clobbers = vec![SYSV_INT_RET, RDX];
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
                MInstr::new(AND_RR)
                    .with_dst(then_masked)
                    .with_vreg(scratch),
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
                MInstr::new(AND_RR)
                    .with_dst(else_masked)
                    .with_vreg(scratch),
            );
            mf.push(
                mblock,
                MInstr::new(MOV_RR).with_dst(dst).with_vreg(then_masked),
            );
            mf.push(
                mblock,
                MInstr::new(OR_RR)
                    .with_dst(dst)
                    .with_vreg(else_masked),
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
            let arg_locs = classify_sysv_args(args.len());
            for (i, &arg_vref) in args.iter().enumerate() {
                let src = res!(arg_vref);
                match arg_locs[i] {
                    ArgLocation::Reg(preg) => {
                        emit_mov_to_preg(mf, mblock, preg, src);
                    }
                    ArgLocation::Stack(off) => {
                        // Stack arguments: use a placeholder store.
                        let _ = off;
                        mf.push(mblock, MInstr::new(PUSH_R).with_vreg(src));
                    }
                }
            }
            let callee_vr = res!(*callee);
            let mut call_mi = MInstr::new(CALL_R).with_vreg(callee_vr);
            call_mi.clobbers = ALLOCATABLE.to_vec();
            mf.push(mblock, call_mi);

            // Capture return value from RAX.
            let dst = new_dst!();
            emit_mov_from_preg(mf, mblock, dst, SYSV_INT_RET);
        }

        // ── memory (placeholder NOP — mem2reg removes most alloca/load/store) ──
        Alloca { .. } | Load { .. } | Store { .. } | GetElementPtr { .. } => {
            let dst = new_dst!();
            mf.push(mblock, MInstr::new(NOP));
            let _ = dst;
        }

        // ── FP arithmetic (not yet supported) ──────────────────────────────
        FAdd { .. } | FSub { .. } | FMul { .. } | FDiv { .. } | FRem { .. } | FNeg { .. } => {
            let dst = new_dst!();
            mf.push(mblock, MInstr::new(MOV_RI).with_dst(dst).with_imm(0));
        }

        // ── aggregate / vector ops (not yet supported) ─────────────────────
        ExtractValue { .. }
        | InsertValue { .. }
        | ExtractElement { .. }
        | InsertElement { .. }
        | ShuffleVector { .. } => {
            let dst = new_dst!();
            mf.push(mblock, MInstr::new(MOV_RI).with_dst(dst).with_imm(0));
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
                emit_mov_to_preg(mf, mblock, SYSV_INT_RET, src);
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
        mf.push(emit_to_mblock, MInstr::new(MOV_RR).with_dst(tmp).with_vreg(src));
        staged.push((dst, tmp));
    }
    for (dst, tmp) in staged {
        mf.push(emit_to_mblock, MInstr::new(MOV_RR).with_dst(dst).with_vreg(tmp));
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
        let mut be = X86Backend;
        let mf = be.lower_function(&ctx, &module, &module.functions[0]);
        assert_eq!(mf.name, "add");
        assert!(!mf.blocks.is_empty());
    }

    #[test]
    fn lower_add_has_ret_instruction() {
        let (ctx, module) = make_add_fn();
        let mut be = X86Backend;
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
        let mut be = X86Backend;
        let mf = be.lower_function(&ctx, &module, &module.functions[0]);
        assert!(!mf.allocatable_pregs.is_empty());
    }

    #[test]
    fn lower_declaration_is_empty() {
        let mut ctx = Context::new();
        let mut module = Module::new("test");
        let mut b = Builder::new(&mut ctx, &mut module);
        b.add_declaration("ext", b.ctx.void_ty, vec![], false);
        let mut be = X86Backend;
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

        let mut be = X86Backend;
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
        let mut be = X86Backend;
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
        let mut be = X86Backend;
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
        let mut be = X86Backend;
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
        let mut be = X86Backend;
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
        let mut be = X86Backend;
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
        let mut be = X86Backend;
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
        let mut be = X86Backend;
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

        let mut be = X86Backend;
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

        let mut be = X86Backend;
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
}
