//! RV64 encoder and object emission.

use crate::instructions::*;
use llvm_codegen::emit::{Emitter, ObjectFormat, Section};
use llvm_codegen::isel::{MInstr, MOpcode, MOperand, MachineFunction, PReg, VReg};

pub struct RiscVEmitter {
    pub format: ObjectFormat,
}

impl RiscVEmitter {
    pub fn new(format: ObjectFormat) -> Self {
        Self { format }
    }
}

impl Emitter for RiscVEmitter {
    fn emit_function(&mut self, mf: &MachineFunction) -> Section {
        let mut data = Vec::new();
        let mut patches: Vec<(usize, usize, PatchKind, MOpcode)> = Vec::new();

        for block in &mf.blocks {
            for instr in &block.instrs {
                let off = data.len();
                if let Some((target, kind)) = patch_from_instr(instr) {
                    patches.push((off, target, kind, instr.opcode));
                }
                let w = encode_instr(instr);
                data.extend_from_slice(&w.to_le_bytes());
            }
        }

        for (off, target_block, kind, opcode) in patches {
            let target_off = mf
                .blocks
                .iter()
                .take(target_block)
                .map(|b| b.instrs.len() * 4)
                .sum::<usize>();
            let rel = (target_off as i64) - (off as i64);
            let old = u32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]]);
            let new = match kind {
                PatchKind::Branch13 => {
                    let rs2 = ((old >> 20) & 0x1F) as u8;
                    let rs1 = ((old >> 15) & 0x1F) as u8;
                    let f3 = (old >> 12) & 0x7;
                    enc_b(rel as i32, rs2, rs1, f3, 0x63)
                }
                PatchKind::Jal21 => {
                    let rd = ((old >> 7) & 0x1F) as u8;
                    enc_j(rel as i32, rd, 0x6F)
                }
            };
            data[off..off + 4].copy_from_slice(&new.to_le_bytes());
            let _ = opcode;
        }

        Section {
            name: ".text".into(),
            data,
            relocs: Vec::new(),
        }
    }

    fn object_format(&self) -> ObjectFormat {
        self.format
    }

    fn elf_machine(&self) -> u16 {
        243 // EM_RISCV
    }
}

#[derive(Clone, Copy)]
enum PatchKind {
    Branch13,
    Jal21,
}

fn patch_from_instr(instr: &MInstr) -> Option<(usize, PatchKind)> {
    let op = instr.opcode;
    if matches!(op, BEQ | BNE | BLT | BGE | BLTU | BGEU) {
        if let Some(MOperand::Block(b)) = instr.operands.get(2) {
            return Some((*b, PatchKind::Branch13));
        }
    }
    if op == JAL {
        if let Some(MOperand::Block(b)) = instr.operands.first() {
            return Some((*b, PatchKind::Jal21));
        }
    }
    None
}

fn reg_of_dst(v: VReg) -> u8 {
    (v.0 & 0x1F) as u8
}

fn reg_of_op(op: &MOperand) -> Option<u8> {
    match op {
        MOperand::PReg(PReg(r)) => Some(*r & 0x1F),
        MOperand::VReg(VReg(v)) => Some((v & 0x1F) as u8),
        _ => None,
    }
}

fn imm_of_op(op: Option<&MOperand>) -> i32 {
    if let Some(MOperand::Imm(v)) = op {
        *v as i32
    } else {
        0
    }
}

fn enc_r(f7: u32, rs2: u8, rs1: u8, f3: u32, rd: u8, opc: u32) -> u32 {
    (f7 << 25)
        | ((rs2 as u32) << 20)
        | ((rs1 as u32) << 15)
        | (f3 << 12)
        | ((rd as u32) << 7)
        | opc
}

fn enc_i(imm: i32, rs1: u8, f3: u32, rd: u8, opc: u32) -> u32 {
    let i = (imm as u32) & 0xFFF;
    (i << 20) | ((rs1 as u32) << 15) | (f3 << 12) | ((rd as u32) << 7) | opc
}

fn enc_s(imm: i32, rs2: u8, rs1: u8, f3: u32, opc: u32) -> u32 {
    let i = (imm as u32) & 0xFFF;
    let hi = (i >> 5) & 0x7F;
    let lo = i & 0x1F;
    (hi << 25)
        | ((rs2 as u32) << 20)
        | ((rs1 as u32) << 15)
        | (f3 << 12)
        | (lo << 7)
        | opc
}

fn enc_b(imm: i32, rs2: u8, rs1: u8, f3: u32, opc: u32) -> u32 {
    let i = (imm as u32) & 0x1FFF;
    let b12 = (i >> 12) & 0x1;
    let b10_5 = (i >> 5) & 0x3F;
    let b4_1 = (i >> 1) & 0xF;
    let b11 = (i >> 11) & 0x1;
    (b12 << 31)
        | (b10_5 << 25)
        | ((rs2 as u32) << 20)
        | ((rs1 as u32) << 15)
        | (f3 << 12)
        | (b4_1 << 8)
        | (b11 << 7)
        | opc
}

fn enc_u(imm: i32, rd: u8, opc: u32) -> u32 {
    ((imm as u32) & 0xFFFFF000) | ((rd as u32) << 7) | opc
}

fn enc_j(imm: i32, rd: u8, opc: u32) -> u32 {
    let i = (imm as u32) & 0x1FFFFF;
    let b20 = (i >> 20) & 0x1;
    let b10_1 = (i >> 1) & 0x3FF;
    let b11 = (i >> 11) & 0x1;
    let b19_12 = (i >> 12) & 0xFF;
    (b20 << 31) | (b10_1 << 21) | (b11 << 20) | (b19_12 << 12) | ((rd as u32) << 7) | opc
}

fn encode_instr(instr: &MInstr) -> u32 {
    let rd = instr.dst.map(reg_of_dst).unwrap_or(0);
    let rs1 = instr.operands.first().and_then(reg_of_op).unwrap_or(0);
    let rs2 = instr.operands.get(1).and_then(reg_of_op).unwrap_or(0);

    match instr.opcode {
        NOP => 0x00000013,
        MOV_RR => enc_i(0, rs1, 0x0, rd, 0x13),
        MOV_IMM => enc_i(imm_of_op(instr.operands.first()), 0, 0x0, rd, 0x13),
        MOV_PR => {
            // operands[0]=PReg(dst), operands[1]=src
            let d = instr.operands.first().and_then(reg_of_op).unwrap_or(0);
            let s = instr.operands.get(1).and_then(reg_of_op).unwrap_or(0);
            enc_i(0, s, 0x0, d, 0x13)
        }

        ADD_RR => enc_r(0x00, rs2, rs1, 0x0, rd, 0x33),
        SUB_RR => enc_r(0x20, rs2, rs1, 0x0, rd, 0x33),
        MUL_RR => enc_r(0x01, rs2, rs1, 0x0, rd, 0x33),
        DIV_RR => enc_r(0x01, rs2, rs1, 0x4, rd, 0x33),
        UDIV_RR => enc_r(0x01, rs2, rs1, 0x5, rd, 0x33),
        REM_RR => enc_r(0x01, rs2, rs1, 0x6, rd, 0x33),
        UREM_RR => enc_r(0x01, rs2, rs1, 0x7, rd, 0x33),
        AND_RR => enc_r(0x00, rs2, rs1, 0x7, rd, 0x33),
        OR_RR => enc_r(0x00, rs2, rs1, 0x6, rd, 0x33),
        XOR_RR => enc_r(0x00, rs2, rs1, 0x4, rd, 0x33),
        SLL_RR => enc_r(0x00, rs2, rs1, 0x1, rd, 0x33),
        SRL_RR => enc_r(0x00, rs2, rs1, 0x5, rd, 0x33),
        SRA_RR => enc_r(0x20, rs2, rs1, 0x5, rd, 0x33),
        SLT_RR => enc_r(0x00, rs2, rs1, 0x2, rd, 0x33),
        SLTU_RR => enc_r(0x00, rs2, rs1, 0x3, rd, 0x33),

        ADDI => enc_i(imm_of_op(instr.operands.get(1)), rs1, 0x0, rd, 0x13),
        XORI => enc_i(imm_of_op(instr.operands.get(1)), rs1, 0x4, rd, 0x13),
        SLTIU => enc_i(imm_of_op(instr.operands.get(1)), rs1, 0x3, rd, 0x13),

        LW => enc_i(imm_of_op(instr.operands.get(2)), rs1, 0x2, rd, 0x03),
        LD => enc_i(imm_of_op(instr.operands.get(2)), rs1, 0x3, rd, 0x03),
        SW => enc_s(imm_of_op(instr.operands.get(2)), rs2, rs1, 0x2, 0x23),
        SD => enc_s(imm_of_op(instr.operands.get(2)), rs2, rs1, 0x3, 0x23),

        BEQ => enc_b(imm_of_op(instr.operands.get(2)), rs2, rs1, 0x0, 0x63),
        BNE => enc_b(imm_of_op(instr.operands.get(2)), rs2, rs1, 0x1, 0x63),
        BLT => enc_b(imm_of_op(instr.operands.get(2)), rs2, rs1, 0x4, 0x63),
        BGE => enc_b(imm_of_op(instr.operands.get(2)), rs2, rs1, 0x5, 0x63),
        BLTU => enc_b(imm_of_op(instr.operands.get(2)), rs2, rs1, 0x6, 0x63),
        BGEU => enc_b(imm_of_op(instr.operands.get(2)), rs2, rs1, 0x7, 0x63),

        JAL => enc_j(imm_of_op(instr.operands.first()), rd, 0x6F),
        JALR => {
            let d = instr.dst.map(reg_of_dst).unwrap_or_else(|| instr.operands.first().and_then(reg_of_op).unwrap_or(0));
            let base_idx = if instr.dst.is_some() { 0 } else { 1 };
            let base = instr.operands.get(base_idx).and_then(reg_of_op).unwrap_or(0);
            let imm = imm_of_op(instr.operands.get(base_idx + 1));
            enc_i(imm, base, 0x0, d, 0x67)
        }
        RET => enc_i(0, 1, 0x0, 0, 0x67),

        LUI => enc_u(imm_of_op(instr.operands.first()), rd, 0x37),
        AUIPC => enc_u(imm_of_op(instr.operands.first()), rd, 0x17),

        _ => panic!("unsupported RISC-V opcode {:?}", instr.opcode),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use llvm_codegen::emit::{emit_object, ObjectFormat};
    use llvm_codegen::isel::{MInstr, MachineFunction};

    fn rr(op: llvm_codegen::isel::MOpcode) -> MInstr {
        MInstr::new(op)
            .with_dst(VReg(3))
            .with_preg(PReg(1))
            .with_preg(PReg(2))
    }

    #[test]
    fn enc_r_add() { assert_eq!(encode_instr(&rr(ADD_RR)), enc_r(0x00, 2, 1, 0, 3, 0x33)); }
    #[test]
    fn enc_r_sub() { assert_eq!(encode_instr(&rr(SUB_RR)), enc_r(0x20, 2, 1, 0, 3, 0x33)); }
    #[test]
    fn enc_r_mul() { assert_eq!(encode_instr(&rr(MUL_RR)), enc_r(0x01, 2, 1, 0, 3, 0x33)); }
    #[test]
    fn enc_r_div() { assert_eq!(encode_instr(&rr(DIV_RR)), enc_r(0x01, 2, 1, 4, 3, 0x33)); }
    #[test]
    fn enc_r_udiv() { assert_eq!(encode_instr(&rr(UDIV_RR)), enc_r(0x01, 2, 1, 5, 3, 0x33)); }
    #[test]
    fn enc_r_rem() { assert_eq!(encode_instr(&rr(REM_RR)), enc_r(0x01, 2, 1, 6, 3, 0x33)); }
    #[test]
    fn enc_r_urem() { assert_eq!(encode_instr(&rr(UREM_RR)), enc_r(0x01, 2, 1, 7, 3, 0x33)); }
    #[test]
    fn enc_r_and() { assert_eq!(encode_instr(&rr(AND_RR)), enc_r(0x00, 2, 1, 7, 3, 0x33)); }
    #[test]
    fn enc_r_or() { assert_eq!(encode_instr(&rr(OR_RR)), enc_r(0x00, 2, 1, 6, 3, 0x33)); }
    #[test]
    fn enc_r_xor() { assert_eq!(encode_instr(&rr(XOR_RR)), enc_r(0x00, 2, 1, 4, 3, 0x33)); }
    #[test]
    fn enc_r_sll() { assert_eq!(encode_instr(&rr(SLL_RR)), enc_r(0x00, 2, 1, 1, 3, 0x33)); }
    #[test]
    fn enc_r_srl() { assert_eq!(encode_instr(&rr(SRL_RR)), enc_r(0x00, 2, 1, 5, 3, 0x33)); }
    #[test]
    fn enc_r_sra() { assert_eq!(encode_instr(&rr(SRA_RR)), enc_r(0x20, 2, 1, 5, 3, 0x33)); }
    #[test]
    fn enc_r_slt() { assert_eq!(encode_instr(&rr(SLT_RR)), enc_r(0x00, 2, 1, 2, 3, 0x33)); }
    #[test]
    fn enc_r_sltu() { assert_eq!(encode_instr(&rr(SLTU_RR)), enc_r(0x00, 2, 1, 3, 3, 0x33)); }

    #[test]
    fn enc_mov_rr() {
        let mi = MInstr::new(MOV_RR).with_dst(VReg(3)).with_preg(PReg(1));
        assert_eq!(encode_instr(&mi), enc_i(0, 1, 0, 3, 0x13));
    }
    #[test]
    fn enc_mov_imm() {
        let mi = MInstr::new(MOV_IMM).with_dst(VReg(3)).with_imm(9);
        assert_eq!(encode_instr(&mi), enc_i(9, 0, 0, 3, 0x13));
    }
    #[test]
    fn enc_mov_pr() {
        let mi = MInstr::new(MOV_PR).with_preg(PReg(10)).with_preg(PReg(5));
        assert_eq!(encode_instr(&mi), enc_i(0, 5, 0, 10, 0x13));
    }

    #[test]
    fn enc_i_addi() {
        let mi = MInstr::new(ADDI).with_dst(VReg(1)).with_preg(PReg(2)).with_imm(7);
        assert_eq!(encode_instr(&mi), enc_i(7, 2, 0, 1, 0x13));
    }
    #[test]
    fn enc_i_xori() {
        let mi = MInstr::new(XORI).with_dst(VReg(1)).with_preg(PReg(2)).with_imm(1);
        assert_eq!(encode_instr(&mi), enc_i(1, 2, 4, 1, 0x13));
    }
    #[test]
    fn enc_i_sltiu() {
        let mi = MInstr::new(SLTIU).with_dst(VReg(1)).with_preg(PReg(2)).with_imm(1);
        assert_eq!(encode_instr(&mi), enc_i(1, 2, 3, 1, 0x13));
    }

    #[test]
    fn enc_i_lw() {
        let mi = MInstr::new(LW).with_dst(VReg(3)).with_preg(PReg(1)).with_preg(PReg(0)).with_imm(16);
        assert_eq!(encode_instr(&mi), enc_i(16, 1, 2, 3, 0x03));
    }
    #[test]
    fn enc_i_ld() {
        let mi = MInstr::new(LD).with_dst(VReg(3)).with_preg(PReg(1)).with_preg(PReg(0)).with_imm(24);
        assert_eq!(encode_instr(&mi), enc_i(24, 1, 3, 3, 0x03));
    }
    #[test]
    fn enc_s_sw() {
        let mi = MInstr::new(SW).with_preg(PReg(1)).with_preg(PReg(2)).with_imm(20);
        assert_eq!(encode_instr(&mi), enc_s(20, 2, 1, 2, 0x23));
    }
    #[test]
    fn enc_s_sd() {
        let mi = MInstr::new(SD).with_preg(PReg(1)).with_preg(PReg(2)).with_imm(28);
        assert_eq!(encode_instr(&mi), enc_s(28, 2, 1, 3, 0x23));
    }

    #[test]
    fn enc_b_beq() {
        let mi = MInstr::new(BEQ).with_preg(PReg(1)).with_preg(PReg(2)).with_imm(16);
        assert_eq!(encode_instr(&mi), enc_b(16, 2, 1, 0, 0x63));
    }
    #[test]
    fn enc_b_bne() {
        let mi = MInstr::new(BNE).with_preg(PReg(1)).with_preg(PReg(2)).with_imm(16);
        assert_eq!(encode_instr(&mi), enc_b(16, 2, 1, 1, 0x63));
    }
    #[test]
    fn enc_b_blt() {
        let mi = MInstr::new(BLT).with_preg(PReg(1)).with_preg(PReg(2)).with_imm(16);
        assert_eq!(encode_instr(&mi), enc_b(16, 2, 1, 4, 0x63));
    }
    #[test]
    fn enc_b_bge() {
        let mi = MInstr::new(BGE).with_preg(PReg(1)).with_preg(PReg(2)).with_imm(16);
        assert_eq!(encode_instr(&mi), enc_b(16, 2, 1, 5, 0x63));
    }
    #[test]
    fn enc_b_bltu() {
        let mi = MInstr::new(BLTU).with_preg(PReg(1)).with_preg(PReg(2)).with_imm(16);
        assert_eq!(encode_instr(&mi), enc_b(16, 2, 1, 6, 0x63));
    }
    #[test]
    fn enc_b_bgeu() {
        let mi = MInstr::new(BGEU).with_preg(PReg(1)).with_preg(PReg(2)).with_imm(16);
        assert_eq!(encode_instr(&mi), enc_b(16, 2, 1, 7, 0x63));
    }

    #[test]
    fn enc_j_jal() {
        let mi = MInstr::new(JAL).with_dst(VReg(1)).with_imm(32);
        assert_eq!(encode_instr(&mi), enc_j(32, 1, 0x6F));
    }
    #[test]
    fn enc_i_jalr() {
        let mi = MInstr::new(JALR).with_dst(VReg(1)).with_preg(PReg(2)).with_imm(12);
        assert_eq!(encode_instr(&mi), enc_i(12, 2, 0, 1, 0x67));
    }
    #[test]
    fn enc_ret() { assert_eq!(encode_instr(&MInstr::new(RET)), enc_i(0, 1, 0, 0, 0x67)); }

    #[test]
    fn enc_u_lui() {
        let mi = MInstr::new(LUI).with_dst(VReg(1)).with_imm(0x12345000);
        assert_eq!(encode_instr(&mi), enc_u(0x12345000, 1, 0x37));
    }
    #[test]
    fn enc_u_auipc() {
        let mi = MInstr::new(AUIPC).with_dst(VReg(1)).with_imm(0x1000);
        assert_eq!(encode_instr(&mi), enc_u(0x1000, 1, 0x17));
    }

    #[test]
    fn helper_i_sign_wrap() { assert_eq!(enc_i(-1, 1, 0, 2, 0x13) >> 20, 0xFFF); }
    #[test]
    fn helper_s_splits_imm() { assert_eq!((enc_s(0x7F, 2, 1, 0, 0x23) >> 7) & 0x1F, 0x1F); }
    #[test]
    fn helper_b_encodes_bit11() { assert_eq!((enc_b(0x800, 2, 1, 0, 0x63) >> 7) & 1, 1); }
    #[test]
    fn helper_j_encodes_bit20() { assert_eq!((enc_j(0x100000, 1, 0x6F) >> 31) & 1, 1); }
    #[test]
    fn helper_u_keeps_upper_20() { assert_eq!(enc_u(0xABCD1000u32 as i32, 1, 0x37) & 0xFFFFF000, 0xABCD1000); }

    #[test]
    fn emitter_outputs_words() {
        let mut mf = MachineFunction::new("f".into());
        let b = mf.add_block("entry");
        mf.push(b, rr(ADD_RR));
        mf.push(b, MInstr::new(RET));
        let mut e = RiscVEmitter::new(ObjectFormat::Elf);
        let sec = e.emit_function(&mf);
        assert_eq!(sec.data.len(), 8);
        assert!(sec.relocs.is_empty());
    }

    #[test]
    fn emitter_branch_patch() {
        let mut mf = MachineFunction::new("f".into());
        let b0 = mf.add_block("b0");
        let b1 = mf.add_block("b1");
        mf.push(b0, MInstr::new(JAL).with_block(b1));
        mf.push(b1, MInstr::new(RET));
        let mut e = RiscVEmitter::new(ObjectFormat::Elf);
        let sec = e.emit_function(&mf);
        let w0 = u32::from_le_bytes([sec.data[0], sec.data[1], sec.data[2], sec.data[3]]);
        assert_eq!(w0 & 0x7f, 0x6f);
        assert_ne!(w0 & 0xFFFFF000, 0);
    }

    #[test]
    fn elf_object_has_riscv_machine_type() {
        let mut mf = MachineFunction::new("f".into());
        let b = mf.add_block("entry");
        mf.push(b, MInstr::new(NOP));
        let mut e = RiscVEmitter::new(ObjectFormat::Elf);
        let obj = emit_object(&mf, &mut e);
        let bytes = obj.to_bytes();
        let e_machine = u16::from_le_bytes([bytes[18], bytes[19]]);
        assert_eq!(e_machine, 243, "EM_RISCV");
    }

    #[test]
    #[should_panic(expected = "unsupported RISC-V opcode")]
    fn enc_unknown_opcode_panics() {
        let _ = encode_instr(&MInstr::new(llvm_codegen::isel::MOpcode(0xFFFF)));
    }
}
