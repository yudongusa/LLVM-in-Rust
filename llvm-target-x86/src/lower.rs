//! x86_64 IR → machine-IR lowering.
//!
//! Implements [`IselBackend`] for [`X86Backend`].  Each IR instruction is
//! translated to one or more machine instructions using virtual registers.
//! Phi-destruction (parallel copy insertion) is also handled here.

use std::collections::HashMap;
use llvm_codegen::isel::{IselBackend, MachineFunction, MInstr, PReg, VReg};
use llvm_ir::{
    ArgId, BlockId, ConstantData, Context, Function, InstrId, InstrKind,
    IntPredicate, Module, ValueRef,
};
use crate::{
    abi::{classify_sysv_args, ArgLocation, SYSV_INT_RET},
    instructions::*,
    regs::{ALLOCATABLE, CALLEE_SAVED, RCX, RDX},
};

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
        IntPredicate::Eq  => CC_EQ,
        IntPredicate::Ne  => CC_NE,
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
            shift_mi.clobbers  = vec![RCX];
            mf.push(mblock, shift_mi);
        }};
    }

    match &instr.kind {
        // ── arithmetic ─────────────────────────────────────────────────────
        Add { lhs, rhs, .. }  => { emit_binop!(ADD_RR, *lhs, *rhs); }
        Sub { lhs, rhs, .. }  => { emit_binop!(SUB_RR, *lhs, *rhs); }
        Mul { lhs, rhs, .. }  => { emit_binop!(IMUL_RR, *lhs, *rhs); }

        SDiv { lhs, rhs, .. } => {
            let dst = new_dst!();
            let l = res!(*lhs);
            let r = res!(*rhs);
            // mov rax, lhs; cqo; idiv rhs → rax = quotient (signed)
            emit_mov_to_preg(mf, mblock, SYSV_INT_RET, l);
            mf.push(mblock, MInstr::new(CQO));
            let mut div_mi = MInstr::new(IDIV_R).with_vreg(r);
            div_mi.clobbers = vec![SYSV_INT_RET, RDX];
            mf.push(mblock, div_mi);
            emit_mov_from_preg(mf, mblock, dst, SYSV_INT_RET);
        }

        UDiv { lhs, rhs, .. } => {
            let dst = new_dst!();
            let l = res!(*lhs);
            let r = res!(*rhs);
            // mov rax, lhs; xor rdx, rdx; div rhs → rax = quotient (unsigned)
            emit_mov_to_preg(mf, mblock, SYSV_INT_RET, l);
            let zero = mf.fresh_vreg();
            mf.push(mblock, MInstr::new(MOV_RI).with_dst(zero).with_imm(0));
            emit_mov_to_preg(mf, mblock, RDX, zero);
            let mut div_mi = MInstr::new(DIV_R).with_vreg(r);
            div_mi.clobbers = vec![SYSV_INT_RET, RDX];
            mf.push(mblock, div_mi);
            emit_mov_from_preg(mf, mblock, dst, SYSV_INT_RET);
        }

        SRem { lhs, rhs, .. } => {
            let dst = new_dst!();
            let l = res!(*lhs);
            let r = res!(*rhs);
            // mov rax, lhs; cqo; idiv rhs → rdx = remainder (signed)
            emit_mov_to_preg(mf, mblock, SYSV_INT_RET, l);
            mf.push(mblock, MInstr::new(CQO));
            let mut div_mi = MInstr::new(IDIV_R).with_vreg(r);
            div_mi.clobbers = vec![SYSV_INT_RET, RDX];
            mf.push(mblock, div_mi);
            emit_mov_from_preg(mf, mblock, dst, RDX);
        }

        URem { lhs, rhs, .. } => {
            let dst = new_dst!();
            let l = res!(*lhs);
            let r = res!(*rhs);
            // mov rax, lhs; xor rdx, rdx; div rhs → rdx = remainder (unsigned)
            emit_mov_to_preg(mf, mblock, SYSV_INT_RET, l);
            let zero = mf.fresh_vreg();
            mf.push(mblock, MInstr::new(MOV_RI).with_dst(zero).with_imm(0));
            emit_mov_to_preg(mf, mblock, RDX, zero);
            let mut div_mi = MInstr::new(DIV_R).with_vreg(r);
            div_mi.clobbers = vec![SYSV_INT_RET, RDX];
            mf.push(mblock, div_mi);
            emit_mov_from_preg(mf, mblock, dst, RDX);
        }

        // ── bitwise ────────────────────────────────────────────────────────
        And { lhs, rhs } => { emit_binop!(AND_RR, *lhs, *rhs); }
        Or  { lhs, rhs } => { emit_binop!(OR_RR,  *lhs, *rhs); }
        Xor { lhs, rhs } => { emit_binop!(XOR_RR, *lhs, *rhs); }

        // ── shifts ─────────────────────────────────────────────────────────
        // x86 variable shifts require the count in CL (low byte of RCX).
        // emit_shift! (defined above) loads rhs into RCX then emits the shift.
        Shl  { lhs, rhs, .. } => { emit_shift!(SHL_RR, *lhs, *rhs); }
        LShr { lhs, rhs, .. } => { emit_shift!(SHR_RR, *lhs, *rhs); }
        AShr { lhs, rhs, .. } => { emit_shift!(SAR_RR, *lhs, *rhs); }

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
        Select { cond, then_val, else_val } => {
            let dst = new_dst!();
            let c  = res!(*cond);
            let tv = res!(*then_val);
            let fv = res!(*else_val);
            // dst = fv; test cond; setne tmp; if cond != 0 → dst = tv.
            // Emit: mov dst, fv; test c, c; jnz ⟨skip 1 instr⟩; mov dst, tv.
            // Simplified: two MOVs + TEST + SETCC.
            mf.push(mblock, MInstr::new(MOV_RR).with_dst(dst).with_vreg(fv));
            mf.push(mblock, MInstr::new(TEST_RR).with_vreg(c).with_vreg(c));
            // We don't have CMOV yet; use a scratch vreg to hold the
            // conditional: scratch = (c != 0) ? 0xFFFF…F : 0.
            // Then: dst = (dst & ~scratch) | (tv & scratch).
            let scratch = mf.fresh_vreg();
            mf.push(mblock, MInstr::new(SETCC).with_dst(scratch).with_imm(CC_NE));
            // Negate scratch: scratch = 0 - scratch (0→0, 1→-1 = all-ones).
            mf.push(mblock, MInstr::new(NEG_R).with_dst(scratch).with_vreg(scratch));
            // dst = (fv & ~scratch) | (tv & scratch)
            let tmp1 = mf.fresh_vreg();
            let tmp2 = mf.fresh_vreg();
            mf.push(mblock, MInstr::new(MOV_RR).with_dst(tmp1).with_vreg(fv));
            mf.push(mblock, MInstr::new(AND_RR).with_dst(tmp1).with_vreg(tmp1).with_vreg(scratch));
            mf.push(mblock, MInstr::new(NOT_R).with_dst(scratch).with_vreg(scratch));
            mf.push(mblock, MInstr::new(MOV_RR).with_dst(tmp2).with_vreg(tv));
            mf.push(mblock, MInstr::new(AND_RR).with_dst(tmp2).with_vreg(tmp2).with_vreg(scratch));
            mf.push(mblock, MInstr::new(OR_RR).with_dst(dst).with_vreg(tmp1).with_vreg(tmp2));
        }

        // ── phi ────────────────────────────────────────────────────────────
        Phi { .. } => {
            // VReg was pre-allocated; copies are inserted by phi-destruction
            // in lower_terminator.  Nothing to do here.
        }

        // ── casts ──────────────────────────────────────────────────────────
        ZExt { val, .. } | Trunc { val, .. } | BitCast { val, .. }
        | PtrToInt { val, .. } | IntToPtr { val, .. }
        | FPTrunc { val, .. } | FPExt { val, .. }
        | FPToUI { val, .. } | FPToSI { val, .. }
        | UIToFP { val, .. } | SIToFP { val, .. }
        | AddrSpaceCast { val, .. } => {
            let dst = new_dst!();
            let src = res!(*val);
            mf.push(mblock, MInstr::new(MOV_RR).with_dst(dst).with_vreg(src));
        }

        SExt { val, .. } => {
            let dst = new_dst!();
            let src = res!(*val);
            mf.push(mblock, MInstr::new(MOVSX_32).with_dst(dst).with_vreg(src));
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
        ExtractValue { .. } | InsertValue { .. } | ExtractElement { .. }
        | InsertElement { .. } | ShuffleVector { .. } => {
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
            emit_phi_copies(ctx, func, mf, mblock, *dest, vmap);
            mf.push(mblock, MInstr::new(JMP).with_block(dest.0 as usize));
        }

        CondBr { cond, then_dest, else_dest } => {
            let c = resolve(ctx, mf, mblock, vmap, *cond);
            // Phi-destruction copies must come before the branch.
            emit_phi_copies(ctx, func, mf, mblock, *then_dest, vmap);
            emit_phi_copies(ctx, func, mf, mblock, *else_dest, vmap);
            mf.push(mblock, MInstr::new(TEST_RR).with_vreg(c).with_vreg(c));
            mf.push(mblock, MInstr::new(JCC)
                .with_imm(CC_NE)
                .with_block(then_dest.0 as usize));
            mf.push(mblock, MInstr::new(JMP).with_block(else_dest.0 as usize));
        }

        Switch { val, default, cases } => {
            let v = resolve(ctx, mf, mblock, vmap, *val);
            for (case_val, case_dest) in cases {
                let cv = resolve(ctx, mf, mblock, vmap, *case_val);
                emit_phi_copies(ctx, func, mf, mblock, *case_dest, vmap);
                mf.push(mblock, MInstr::new(CMP_RR).with_vreg(v).with_vreg(cv));
                mf.push(mblock, MInstr::new(JCC)
                    .with_imm(CC_EQ)
                    .with_block(case_dest.0 as usize));
            }
            emit_phi_copies(ctx, func, mf, mblock, *default, vmap);
            mf.push(mblock, MInstr::new(JMP).with_block(default.0 as usize));
        }

        Unreachable => {
            mf.push(mblock, MInstr::new(NOP));
        }

        _ => {} // body instructions already handled
    }
}

// ── phi destruction ───────────────────────────────────────────────────────

/// For each phi in `dest`, emit a copy from the incoming value (for the
/// edge from `src_mblock`) into the phi's pre-allocated VReg.
fn emit_phi_copies(
    ctx: &Context,
    func: &Function,
    mf: &mut MachineFunction,
    src_mblock: usize,
    dest: BlockId,
    vmap: &mut HashMap<ValueRef, VReg>,
) {
    let dest_bb = &func.blocks[dest.0 as usize];
    let src_bid = BlockId(src_mblock as u32);

    for &iid in &dest_bb.body {
        if let InstrKind::Phi { incoming, .. } = &func.instr(iid).kind {
            // Find the incoming value from `src_bid`.
            if let Some((incoming_val, _)) = incoming.iter().find(|(_, bid)| *bid == src_bid) {
                let phi_vreg = match vmap.get(&ValueRef::Instruction(iid)) {
                    Some(&v) => v,
                    None => continue,
                };
                let src_vreg = resolve(ctx, mf, src_mblock, vmap, *incoming_val);
                mf.push(src_mblock, MInstr::new(MOV_RR)
                    .with_dst(phi_vreg)
                    .with_vreg(src_vreg));
            }
        }
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
        let has_ret = mf.blocks.iter().any(|b| {
            b.instrs.iter().any(|i| i.opcode == RET)
        });
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

        let has_cmp = mf.blocks.iter().any(|bl| {
            bl.instrs.iter().any(|i| i.opcode == CMP_RR)
        });
        let has_setcc = mf.blocks.iter().any(|bl| {
            bl.instrs.iter().any(|i| i.opcode == SETCC)
        });
        assert!(has_cmp,   "should emit CMP");
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
        let shl_instr = mf.blocks.iter().flat_map(|b| b.instrs.iter())
            .find(|i| i.opcode == SHL_RR)
            .expect("should emit SHL_RR");

        // SHL_RR must declare RCX as a physical use (shift reads CL).
        assert!(shl_instr.phys_uses.contains(&RCX),
            "SHL_RR must have RCX in phys_uses (CL holds the shift amount)");

        // There must be a MOV_PR targeting RCX somewhere in the function.
        let has_mov_to_rcx = mf.blocks.iter().flat_map(|b| b.instrs.iter()).any(|i| {
            i.opcode == MOV_PR && i.operands.first() == Some(&llvm_codegen::isel::MOperand::PReg(RCX))
        });
        assert!(has_mov_to_rcx,
            "a MOV_PR loading the shift count into RCX must be emitted before SHL_RR");
    }

    #[test]
    fn udiv_uses_div_r_not_idiv_r() {
        // Issue #31: UDiv must emit DIV_R (unsigned) not IDIV_R (signed).
        let (ctx, module) = make_div_fn(true);
        let mut be = X86Backend;
        let mf = be.lower_function(&ctx, &module, &module.functions[0]);
        let has_div_r = mf.blocks.iter().any(|bl| bl.instrs.iter().any(|i| i.opcode == DIV_R));
        let has_idiv_r = mf.blocks.iter().any(|bl| bl.instrs.iter().any(|i| i.opcode == IDIV_R));
        assert!(has_div_r,  "UDiv must emit DIV_R (unsigned div)");
        assert!(!has_idiv_r, "UDiv must NOT emit IDIV_R (signed div)");
    }

    #[test]
    fn sdiv_uses_idiv_r() {
        // Regression: SDiv must still emit IDIV_R (signed).
        let (ctx, module) = make_div_fn(false);
        let mut be = X86Backend;
        let mf = be.lower_function(&ctx, &module, &module.functions[0]);
        let has_idiv_r = mf.blocks.iter().any(|bl| bl.instrs.iter().any(|i| i.opcode == IDIV_R));
        let has_div_r  = mf.blocks.iter().any(|bl| bl.instrs.iter().any(|i| i.opcode == DIV_R));
        assert!(has_idiv_r, "SDiv must emit IDIV_R (signed div)");
        assert!(!has_div_r,  "SDiv must NOT emit DIV_R (unsigned div)");
    }

    #[test]
    fn emit_mov_to_preg_uses_mov_pr_opcode() {
        // emit_mov_to_preg must use MOV_PR (not MOV_RR) so that the fixed
        // physical register destination survives register allocation.
        // Verify: the instruction has dst=None, opcode=MOV_PR,
        //         operands[0]=PReg(preg), operands[1]=VReg(src).
        use llvm_codegen::isel::{MachineFunction, MOperand};
        use crate::regs::RAX;

        let mut mf = MachineFunction::new("f".into());
        let b = mf.add_block("entry");
        let src = mf.fresh_vreg();
        super::emit_mov_to_preg(&mut mf, b, RAX, src);

        let instr = &mf.blocks[b].instrs[0];
        assert_eq!(instr.opcode, MOV_PR, "emit_mov_to_preg must use MOV_PR opcode");
        assert!(instr.dst.is_none(), "dst must be None (destination is a fixed PReg)");
        assert_eq!(instr.operands.len(), 2);
        assert_eq!(instr.operands[0], MOperand::PReg(RAX), "operands[0] must be the fixed PReg");
        assert_eq!(instr.operands[1], MOperand::VReg(src), "operands[1] must be the source VReg");
    }

    #[test]
    fn add_binop_instr_has_single_rhs_operand() {
        // After fix for issue #34: the ADD_RR instruction must have exactly
        // one operand (the RHS vreg), not two (self-reference + rhs).
        let (ctx, module) = make_add_fn();
        let mut be = X86Backend;
        let mf = be.lower_function(&ctx, &module, &module.functions[0]);

        // Find the ADD_RR instruction.
        let add_instr = mf.blocks.iter()
            .flat_map(|b| b.instrs.iter())
            .find(|i| i.opcode == ADD_RR);
        let add_instr = add_instr.expect("should have an ADD_RR instruction");

        assert_eq!(
            add_instr.operands.len(), 1,
            "ADD_RR must carry only the RHS operand, not a self-reference (issue #34)"
        );
    }
}
