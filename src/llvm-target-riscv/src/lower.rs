//! RV64 IR -> machine-IR lowering.

use crate::{
    abi::{classify_rv64_int_args, ArgLocation, INT_RET},
    instructions::*,
    regs::{ALLOCATABLE, CALLEE_SAVED, X0},
};
use llvm_codegen::isel::{IselBackend, MInstr, MachineFunction, PReg, VReg};
use llvm_ir::{
    ArgId, BlockId, ConstantData, Context, Function, InstrId, InstrKind, IntPredicate, Module,
    ValueRef,
};
use std::collections::HashMap;

#[derive(Default)]
pub struct RiscVBackend;

impl IselBackend for RiscVBackend {
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
                lower_instr(ctx, module, func, &mut mf, bi, iid, &mut vmap);
            }
            if let Some(tid) = bb.terminator {
                lower_terminator(ctx, func, &mut mf, bi, tid, &mut vmap);
            }
        }

        mf
    }
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
            let vreg = mf.fresh_vreg();
            vmap.insert(vr, vreg);
            let imm = const_to_imm(ctx.get_const(cid));
            mf.push(mblock, MInstr::new(MOV_IMM).with_dst(vreg).with_imm(imm));
            vreg
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

        FCmp { .. } => {
            let dst = new_dst!();
            mf.push(mblock, MInstr::new(MOV_IMM).with_dst(dst).with_imm(0));
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
        | AddrSpaceCast { val, .. }
        | SExt { val, .. } => {
            let dst = new_dst!();
            let src = res!(*val);
            mf.push(mblock, MInstr::new(MOV_RR).with_dst(dst).with_vreg(src));
        }

        Call { callee, args, .. } => {
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

        Store { .. } => {
            mf.push(mblock, MInstr::new(NOP));
        }
        Alloca { .. } | Load { .. } | GetElementPtr { .. } => {
            let dst = new_dst!();
            mf.push(mblock, MInstr::new(MOV_IMM).with_dst(dst).with_imm(0));
        }

        FAdd { .. } | FSub { .. } | FMul { .. } | FDiv { .. } | FRem { .. } | FNeg { .. } => {
            let dst = new_dst!();
            mf.push(mblock, MInstr::new(MOV_IMM).with_dst(dst).with_imm(0));
        }

        ExtractValue { .. }
        | InsertValue { .. }
        | ExtractElement { .. }
        | InsertElement { .. }
        | ShuffleVector { .. } => {
            let dst = new_dst!();
            mf.push(mblock, MInstr::new(MOV_IMM).with_dst(dst).with_imm(0));
        }

        Ret { .. } | Br { .. } | CondBr { .. } | Switch { .. } | Unreachable => {}
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
}
