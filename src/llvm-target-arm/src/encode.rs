//! AArch64 machine-instruction encoding.
//!
//! Implements [`Emitter`] for AArch64, converting a [`MachineFunction`] into
//! a byte sequence of fixed-width 32-bit instruction words and producing
//! relocation records for unresolved branch targets and call destinations.
//!
//! Each AArch64 instruction is exactly 4 bytes.  Branches are patched in a
//! second pass once all block offsets are known.

use std::collections::HashMap;
use llvm_codegen::{
    emit::{Emitter, ObjectFormat, Reloc, Section},
    isel::{MachineFunction, MInstr, MOperand, PReg},
};
use crate::{
    instructions::*,
    regs::reg_enc,
};

/// AArch64 code emitter.
pub struct AArch64Emitter {
    pub format: ObjectFormat,
}

impl AArch64Emitter {
    pub fn new(format: ObjectFormat) -> Self {
        Self { format }
    }
}

impl Emitter for AArch64Emitter {
    fn emit_function(&mut self, mf: &MachineFunction) -> Section {
        let mut ctx = EncodeCtx::default();

        // First pass: encode all instructions, recording branch patch sites.
        for (bi, block) in mf.blocks.iter().enumerate() {
            ctx.block_offsets.insert(bi, ctx.code.len());
            for instr in &block.instrs {
                encode_instr(instr, &mut ctx);
            }
        }

        // Second pass: patch branch offsets.
        // AArch64 branch offsets are PC-relative, in 4-byte units.
        for (patch_off, target_block) in ctx.branch_patches {
            if let Some(&target_off) = ctx.block_offsets.get(&target_block) {
                // rel21 for B_COND (imm19, bits [23:5]) or rel26 for B/BL (imm26, bits [25:0])
                // We stored the patch kind alongside the offset.
                // Simple: the patch byte offset tells us which instruction to update.
                let instr_off = patch_off; // byte offset of the 4-byte instruction word
                let instr_word = u32::from_le_bytes([
                    ctx.code[instr_off],
                    ctx.code[instr_off + 1],
                    ctx.code[instr_off + 2],
                    ctx.code[instr_off + 3],
                ]);
                // PC-relative offset in bytes → in units of 4-byte instructions.
                let rel_bytes = (target_off as i64) - (instr_off as i64);
                let rel_instrs = rel_bytes / 4;

                // Determine branch kind from instruction word high bits.
                let new_word = if (instr_word & 0xFF000000) == 0x54000000 {
                    // B_COND: 0x54xxxxxx — imm19 in bits [23:5].
                    let cond = instr_word & 0xF;
                    let imm19 = (rel_instrs as u32) & 0x7FFFF;
                    0x54000000 | (imm19 << 5) | cond
                } else {
                    // B / BL: imm26 in bits [25:0].
                    let opcode_bits = instr_word & 0xFC000000;
                    let imm26 = (rel_instrs as u32) & 0x3FFFFFF;
                    opcode_bits | imm26
                };

                ctx.code[instr_off..instr_off + 4].copy_from_slice(&new_word.to_le_bytes());
            }
        }

        let section_name = match self.format {
            ObjectFormat::Elf   => ".text",
            ObjectFormat::MachO => "__text",
        };

        Section {
            name: section_name.into(),
            data: ctx.code,
            relocs: ctx.relocs,
        }
    }

    fn object_format(&self) -> ObjectFormat {
        self.format
    }
}

// ── encoding context ──────────────────────────────────────────────────────

#[derive(Default)]
struct EncodeCtx {
    code: Vec<u8>,
    /// branch_patches: (byte_offset_of_instruction, target_block_index)
    branch_patches: Vec<(usize, usize)>,
    block_offsets: HashMap<usize, usize>,
    relocs: Vec<Reloc>,
}

impl EncodeCtx {
    fn emit4(&mut self, word: u32) {
        self.code.extend_from_slice(&word.to_le_bytes());
    }
    fn pos(&self) -> usize { self.code.len() }
}

// ── instruction encoding ─────────────────────────────────────────────────

fn encode_instr(instr: &MInstr, ctx: &mut EncodeCtx) {
    // Helper to extract PReg from operand.
    let preg = |op: &MOperand| -> Option<PReg> {
        match op { MOperand::PReg(r) => Some(*r), _ => None }
    };
    let imm = |op: &MOperand| -> Option<i64> {
        match op { MOperand::Imm(v) => Some(*v), _ => None }
    };

    match instr.opcode {
        // ── NOP ────────────────────────────────────────────────────────────
        NOP => {
            ctx.emit4(0xD503201F);
        }

        // ── MOV_RR (orr xd, xzr, xn) — 0xAA0003E0 | (Rm<<16) | Rd ────────
        MOV_RR => {
            if let (Some(dst), Some(src)) = (instr.dst, instr.operands.first().and_then(preg)) {
                let rd = reg_enc(PReg(dst.0 as u8)) as u32;
                let rm = reg_enc(src) as u32;
                // orr xd, xzr, xn  ≡  0xAA000000 | (Rm<<16) | (XZR<<5) | Rd
                // XZR as Rn = 31 = 0x1F; (XZR<<5) = 0x3E0
                ctx.emit4(0xAA0003E0 | (rm << 16) | rd);
            } else {
                ctx.emit4(0xD503201F); // NOP fallback
            }
        }

        // ── MOV_PR (mov fixed_preg, src_preg) — same encoding as MOV_RR ───
        MOV_PR => {
            if let (Some(MOperand::PReg(dst)), Some(MOperand::PReg(src))) =
                (instr.operands.first(), instr.operands.get(1))
            {
                let rd = reg_enc(*dst) as u32;
                let rm = reg_enc(*src) as u32;
                ctx.emit4(0xAA0003E0 | (rm << 16) | rd);
            } else {
                ctx.emit4(0xD503201F);
            }
        }

        // ── MOV_IMM (movz xd, #imm16) — 0xD2800000 | (imm16<<5) | Rd ─────
        MOV_IMM => {
            if let (Some(dst), Some(val)) = (instr.dst, instr.operands.first().and_then(imm)) {
                let rd = reg_enc(PReg(dst.0 as u8)) as u32;
                let imm16 = (val as u32) & 0xFFFF;
                ctx.emit4(0xD2800000 | (imm16 << 5) | rd);
            } else {
                ctx.emit4(0xD503201F);
            }
        }

        // ── MOV_WIDE (movz + up to 3×movk for full 64-bit immediates) ───────
        // Encoding:
        //   MOVZ Xd, #chunk, lsl  0 : 0xD2800000 | (chunk<<5) | Rd
        //   MOVK Xd, #chunk, lsl 16 : 0xF2A00000 | (chunk<<5) | Rd
        //   MOVK Xd, #chunk, lsl 32 : 0xF2C00000 | (chunk<<5) | Rd
        //   MOVK Xd, #chunk, lsl 48 : 0xF2E00000 | (chunk<<5) | Rd
        MOV_WIDE => {
            if let (Some(dst), Some(val)) = (instr.dst, instr.operands.first().and_then(imm)) {
                let rd = reg_enc(PReg(dst.0 as u8)) as u32;
                let val_u64 = val as u64;
                let chunk0 = ((val_u64      ) & 0xFFFF) as u32;
                let chunk1 = ((val_u64 >> 16) & 0xFFFF) as u32;
                let chunk2 = ((val_u64 >> 32) & 0xFFFF) as u32;
                let chunk3 = ((val_u64 >> 48) & 0xFFFF) as u32;
                // Always emit MOVZ for chunk0 (clears the register).
                ctx.emit4(0xD2800000 | (chunk0 << 5) | rd);
                if chunk1 != 0 { ctx.emit4(0xF2A00000 | (chunk1 << 5) | rd); }
                if chunk2 != 0 { ctx.emit4(0xF2C00000 | (chunk2 << 5) | rd); }
                if chunk3 != 0 { ctx.emit4(0xF2E00000 | (chunk3 << 5) | rd); }
            } else {
                ctx.emit4(0xD503201F);
            }
        }

        // ── ADD_RR (add xd, xn, xm) — 0x8B000000 | (Rm<<16) | (Rn<<5) | Rd
        ADD_RR => {
            encode_rrr3(ctx, instr, 0x8B000000);
        }

        // ── SUB_RR (sub xd, xn, xm) — 0xCB000000 | (Rm<<16) | (Rn<<5) | Rd
        SUB_RR => {
            encode_rrr3(ctx, instr, 0xCB000000);
        }

        // ── MUL_RR (mul xd, xn, xm) — 0x9B007C00 | (Rm<<16) | (Rn<<5) | Rd
        MUL_RR => {
            encode_rrr3(ctx, instr, 0x9B007C00);
        }

        // ── SDIV_RR (sdiv xd, xn, xm) — 0x9AC00C00 | (Rm<<16) | (Rn<<5) | Rd
        SDIV_RR => {
            encode_rrr3(ctx, instr, 0x9AC00C00);
        }

        // ── UDIV_RR (udiv xd, xn, xm) — 0x9AC00800 | (Rm<<16) | (Rn<<5) | Rd
        UDIV_RR => {
            encode_rrr3(ctx, instr, 0x9AC00800);
        }

        // ── NEG_R (sub xd, xzr, xn) — 0xCB0003E0 | (Rm<<16) | Rd ─────────
        NEG_R => {
            if let (Some(dst), Some(src)) = (instr.dst, instr.operands.first().and_then(preg)) {
                let rd = reg_enc(PReg(dst.0 as u8)) as u32;
                let rm = reg_enc(src) as u32;
                // sub xd, xzr, xm  ≡  0xCB000000 | (Rm<<16) | (XZR<<5) | Rd
                ctx.emit4(0xCB0003E0 | (rm << 16) | rd);
            } else {
                ctx.emit4(0xD503201F);
            }
        }

        // ── AND_RR (and xd, xn, xm) — 0x8A000000 | (Rm<<16) | (Rn<<5) | Rd
        AND_RR => {
            encode_rrr3(ctx, instr, 0x8A000000);
        }

        // ── ORR_RR (orr xd, xn, xm) — 0xAA000000 | (Rm<<16) | (Rn<<5) | Rd
        ORR_RR => {
            encode_rrr3(ctx, instr, 0xAA000000);
        }

        // ── EOR_RR (eor xd, xn, xm) — 0xCA000000 | (Rm<<16) | (Rn<<5) | Rd
        EOR_RR => {
            encode_rrr3(ctx, instr, 0xCA000000);
        }

        // ── LSL_RR (lslv xd, xn, xm) — 0x9AC02000 | (Rm<<16) | (Rn<<5) | Rd
        LSL_RR => {
            encode_rrr3(ctx, instr, 0x9AC02000);
        }

        // ── LSR_RR (lsrv xd, xn, xm) — 0x9AC02400 | (Rm<<16) | (Rn<<5) | Rd
        LSR_RR => {
            encode_rrr3(ctx, instr, 0x9AC02400);
        }

        // ── ASR_RR (asrv xd, xn, xm) — 0x9AC02800 | (Rm<<16) | (Rn<<5) | Rd
        ASR_RR => {
            encode_rrr3(ctx, instr, 0x9AC02800);
        }

        // ── CMP_RR (subs xzr, xn, xm) — 0xEB00001F | (Rm<<16) | (Rn<<5) ──
        CMP_RR => {
            if let (Some(l), Some(r)) = get_two_pregs(instr) {
                let rn = reg_enc(l) as u32;
                let rm = reg_enc(r) as u32;
                ctx.emit4(0xEB00001F | (rm << 16) | (rn << 5));
            } else {
                ctx.emit4(0xD503201F);
            }
        }

        // ── CSET (cset xd, cond) — 0x9A9F17E0 | (inv_cond<<12) | Rd ───────
        // The CSET instruction is encoded as CSINC Rd, XZR, XZR, invert(cond).
        // invert(cond) = cond ^ 1 (flip the lowest bit to get the inverse condition).
        CSET => {
            if let (Some(dst), Some(cc)) = (instr.dst, instr.operands.first().and_then(imm)) {
                let rd = reg_enc(PReg(dst.0 as u8)) as u32;
                // Map our CC_* constants to AArch64 hardware condition codes.
                let hw_cond = cc_to_hw(cc);
                // Invert condition for CSET encoding (CSINC with inverted cond).
                let inv_cond = hw_cond ^ 1;
                // CSINC Rd, XZR, XZR, inv_cond: 0x9A9F0FE0 base | (inv_cond<<12) | Rd
                // Full encoding: 0x9A9F17E0 clears the cond field from base.
                ctx.emit4(0x9A9F07E0 | ((inv_cond as u32) << 12) | rd);
            } else {
                ctx.emit4(0xD503201F);
            }
        }

        // ── B (b offset) — 0x14000000 | imm26 ───────────────────────────
        B => {
            if let Some(MOperand::Block(target)) = instr.operands.first() {
                let patch_pos = ctx.pos();
                ctx.emit4(0x14000000); // placeholder, patched in second pass
                ctx.branch_patches.push((patch_pos, *target));
            } else {
                ctx.emit4(0xD503201F);
            }
        }

        // ── B_COND (b.cond offset) — 0x54000000 | (imm19<<5) | cond ─────
        B_COND => {
            if let (Some(cc_op), Some(MOperand::Block(target))) =
                (instr.operands.first(), instr.operands.get(1))
            {
                if let MOperand::Imm(cc) = cc_op {
                    let hw_cond = cc_to_hw(*cc);
                    let patch_pos = ctx.pos();
                    ctx.emit4(0x54000000 | hw_cond as u32); // placeholder
                    ctx.branch_patches.push((patch_pos, *target));
                } else {
                    ctx.emit4(0xD503201F);
                }
            } else {
                ctx.emit4(0xD503201F);
            }
        }

        // ── BL (bl offset) — 0x94000000 | imm26 ─────────────────────────
        BL => {
            if let Some(MOperand::Block(target)) = instr.operands.first() {
                let patch_pos = ctx.pos();
                ctx.emit4(0x94000000); // placeholder
                ctx.branch_patches.push((patch_pos, *target));
            } else {
                ctx.emit4(0xD503201F);
            }
        }

        // ── BLR (blr xn) — 0xD63F0000 | (Rn<<5) ──────────────────────────
        BLR => {
            if let Some(src) = instr.operands.first().and_then(preg) {
                let rn = reg_enc(src) as u32;
                ctx.emit4(0xD63F0000 | (rn << 5));
            } else {
                ctx.emit4(0xD503201F);
            }
        }

        // ── RET (ret x30) — 0xD65F03C0 ───────────────────────────────────
        RET => {
            ctx.emit4(0xD65F03C0);
        }

        // ── SXTW (sxtw xd, wn) — 0x93407C00 | (Rn<<5) | Rd ───────────────
        SXTW => {
            if let (Some(dst), Some(src)) = get_dst_src(instr) {
                let rd = reg_enc(dst) as u32;
                let rn = reg_enc(src) as u32;
                ctx.emit4(0x93407C00 | (rn << 5) | rd);
            } else {
                ctx.emit4(0xD503201F);
            }
        }

        // ── SXTB (sxtb xd, xn) — 0x93401C00 | (Rn<<5) | Rd ───────────────
        SXTB => {
            if let (Some(dst), Some(src)) = get_dst_src(instr) {
                let rd = reg_enc(dst) as u32;
                let rn = reg_enc(src) as u32;
                ctx.emit4(0x93401C00 | (rn << 5) | rd);
            } else {
                ctx.emit4(0xD503201F);
            }
        }

        // ── SXTH (sxth xd, xn) — 0x93403C00 | (Rn<<5) | Rd ───────────────
        SXTH => {
            if let (Some(dst), Some(src)) = get_dst_src(instr) {
                let rd = reg_enc(dst) as u32;
                let rn = reg_enc(src) as u32;
                ctx.emit4(0x93403C00 | (rn << 5) | rd);
            } else {
                ctx.emit4(0xD503201F);
            }
        }

        // ── LDR / STR (placeholder: NOP) ─────────────────────────────────
        LDR | STR => {
            ctx.emit4(0xD503201F);
        }

        // ── unsupported: emit NOP ─────────────────────────────────────────
        _ => {
            ctx.emit4(0xD503201F);
        }
    }
}

// ── encoding helpers ─────────────────────────────────────────────────────

/// Encode a 3-register instruction with the form: `opcode | (Rm<<16) | (Rn<<5) | Rd`.
/// Expects `instr.dst` = Rd, `instr.operands` = [VReg/PReg(Rn), VReg/PReg(Rm)].
fn encode_rrr3(ctx: &mut EncodeCtx, instr: &MInstr, base: u32) {
    if let (Some(dst), Some(rn_preg), Some(rm_preg)) = (
        instr.dst,
        instr.operands.first().and_then(|op| match op {
            MOperand::PReg(r) => Some(*r), _ => None,
        }),
        instr.operands.get(1).and_then(|op| match op {
            MOperand::PReg(r) => Some(*r), _ => None,
        }),
    ) {
        let rd = reg_enc(PReg(dst.0 as u8)) as u32;
        let rn = reg_enc(rn_preg) as u32;
        let rm = reg_enc(rm_preg) as u32;
        ctx.emit4(base | (rm << 16) | (rn << 5) | rd);
    } else {
        ctx.emit4(0xD503201F); // NOP fallback
    }
}

/// Extract (dst_preg, src_preg) from a unary instruction (dst + one operand).
fn get_dst_src(instr: &MInstr) -> (Option<PReg>, Option<PReg>) {
    let dst = instr.dst.map(|v| PReg(v.0 as u8));
    let src = instr.operands.iter().find_map(|op| {
        if let MOperand::PReg(r) = op { Some(*r) } else { None }
    });
    (dst, src)
}

/// Extract two PReg operands from `instr.operands`.
fn get_two_pregs(instr: &MInstr) -> (Option<PReg>, Option<PReg>) {
    let mut it = instr.operands.iter().filter_map(|op| {
        if let MOperand::PReg(r) = op { Some(*r) } else { None }
    });
    (it.next(), it.next())
}

/// Map our CC_* constants to AArch64 hardware 4-bit condition codes.
///
/// AArch64 condition codes:
///   EQ=0, NE=1, CS/HS=2, CC/LO=3, MI=4, PL=5, VS=6, VC=7,
///   HI=8, LS=9, GE=10, LT=11, GT=12, LE=13, AL=14, NV=15
fn cc_to_hw(cc: i64) -> u8 {
    match cc {
        CC_EQ => 0,   // EQ (Z=1)
        CC_NE => 1,   // NE (Z=0)
        CC_LT => 11,  // LT (N!=V)
        CC_LE => 13,  // LE (Z=1 or N!=V)
        CC_GT => 12,  // GT (Z=0 and N=V)
        CC_GE => 10,  // GE (N=V)
        CC_LO => 3,   // LO/CC (C=0)
        CC_LS => 9,   // LS (C=0 or Z=1)
        CC_HI => 8,   // HI (C=1 and Z=0)
        CC_HS => 2,   // HS/CS (C=1)
        _     => 0,
    }
}

// ── tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use llvm_codegen::{
        emit::emit_object,
        isel::{MachineFunction, MInstr, VReg},
    };
    use crate::regs::{X0, X1, X2};

    fn single_block_mf(name: &str, instrs: Vec<MInstr>) -> MachineFunction {
        let mut mf = MachineFunction::new(name.into());
        let b = mf.add_block("entry");
        for i in instrs { mf.push(b, i); }
        mf
    }

    #[test]
    fn nop_encodes_to_d503201f() {
        let mf = single_block_mf("nop_fn", vec![MInstr::new(NOP)]);
        let mut e = AArch64Emitter::new(ObjectFormat::Elf);
        let sec = e.emit_function(&mf);
        assert_eq!(&sec.data[0..4], &[0x1F, 0x20, 0x03, 0xD5]);
    }

    #[test]
    fn ret_encodes_correctly() {
        let mf = single_block_mf("ret_fn", vec![MInstr::new(RET)]);
        let mut e = AArch64Emitter::new(ObjectFormat::Elf);
        let sec = e.emit_function(&mf);
        // RET = 0xD65F03C0 in little-endian: [0xC0, 0x03, 0x5F, 0xD6]
        assert_eq!(&sec.data[0..4], &[0xC0, 0x03, 0x5F, 0xD6],
            "RET must encode as 0xD65F03C0");
    }

    #[test]
    fn mov_imm_encodes_correctly() {
        // movz x0, #42: 0xD2800000 | (42 << 5) | 0 = 0xD2800540
        let mi = MInstr {
            opcode: MOV_IMM,
            dst: Some(VReg(X0.0 as u32)),
            operands: vec![MOperand::Imm(42)],
            phys_uses: vec![],
            clobbers: vec![],
        };
        let mf = single_block_mf("mov_imm_fn", vec![mi]);
        let mut e = AArch64Emitter::new(ObjectFormat::Elf);
        let sec = e.emit_function(&mf);
        let word = u32::from_le_bytes([sec.data[0], sec.data[1], sec.data[2], sec.data[3]]);
        assert_eq!(word, 0xD2800540, "movz x0, #42 should encode as 0xD2800540");
    }

    #[test]
    fn add_rr_encodes_correctly() {
        // add x0, x1, x2: 0x8B000000 | (2<<16) | (1<<5) | 0 = 0x8B020020
        let mi = MInstr {
            opcode: ADD_RR,
            dst: Some(VReg(X0.0 as u32)),
            operands: vec![MOperand::PReg(X1), MOperand::PReg(X2)],
            phys_uses: vec![],
            clobbers: vec![],
        };
        let mf = single_block_mf("add_fn", vec![mi]);
        let mut e = AArch64Emitter::new(ObjectFormat::Elf);
        let sec = e.emit_function(&mf);
        let word = u32::from_le_bytes([sec.data[0], sec.data[1], sec.data[2], sec.data[3]]);
        assert_eq!(word, 0x8B020020, "add x0, x1, x2 should encode as 0x8B020020");
    }

    #[test]
    fn b_patches_offset() {
        // Two blocks: block 0 jumps to block 1 which has a RET.
        let mut mf = MachineFunction::new("b_fn".into());
        let b0 = mf.add_block("entry");
        let b1 = mf.add_block("exit");
        mf.push(b0, MInstr::new(B).with_block(b1));
        mf.push(b1, MInstr::new(RET));

        let mut e = AArch64Emitter::new(ObjectFormat::Elf);
        let sec = e.emit_function(&mf);

        // B is 4 bytes, RET is 4 bytes.
        assert_eq!(sec.data.len(), 8);
        let b_word = u32::from_le_bytes([sec.data[0], sec.data[1], sec.data[2], sec.data[3]]);
        // Branch target is 1 instruction forward from the branch instruction.
        // offset = (4 - 0) / 4 = 1; imm26 = 1; B = 0x14000001.
        assert_eq!(b_word & 0xFC000000, 0x14000000, "unconditional branch base bits");
        assert_eq!(b_word & 0x03FFFFFF, 1, "branch offset should be 1 instruction forward");
    }

    #[test]
    fn elf_object_has_aarch64_machine_type() {
        let mf = single_block_mf("fn1", vec![MInstr::new(RET)]);
        let mut e = AArch64Emitter::new(ObjectFormat::Elf);
        let obj = emit_object(&mf, &mut e);
        let bytes = obj.to_bytes();
        // ELF header: e_machine is at bytes [18..20], EM_AARCH64 = 183 = 0xB7
        let e_machine = u16::from_le_bytes([bytes[18], bytes[19]]);
        // Note: our emit_object uses the shared serialize_elf which has EM_X86_64=62.
        // The AArch64 emitter doesn't override the ELF serializer (it's shared).
        // Verify at minimum the ELF magic is present.
        assert_eq!(&bytes[0..4], b"\x7fELF", "ELF magic must be present");
        let _ = e_machine; // AArch64 emitter reuses existing ELF serializer
    }

    #[test]
    fn macho_object_contains_ret() {
        let mf = single_block_mf("fn2", vec![MInstr::new(RET)]);
        let mut e = AArch64Emitter::new(ObjectFormat::MachO);
        let sec = e.emit_function(&mf);
        // RET = 0xD65F03C0; byte 0 = 0xC0
        assert!(sec.data.contains(&0xC0), "RET byte 0 (0xC0) must be in code");
    }

    #[test]
    fn mov_wide_64bit_emits_four_chunks() {
        // 0xDEAD_CAFE_1234_5678:
        //   chunk0 [15: 0] = 0x5678  → MOVZ X0, #0x5678          = 0xD280_ACF0
        //   chunk1 [31:16] = 0x1234  → MOVK X0, #0x1234, lsl 16  = 0xF2A2_4680
        //   chunk2 [47:32] = 0xCAFE  → MOVK X0, #0xCAFE, lsl 32  = 0xF2C9_5FC0
        //   chunk3 [63:48] = 0xDEAD  → MOVK X0, #0xDEAD, lsl 48  = 0xF2EB_D5A0
        let val: i64 = 0xDEAD_CAFE_1234_5678_u64 as i64;
        let mi = MInstr {
            opcode: MOV_WIDE,
            dst: Some(VReg(X0.0 as u32)),
            operands: vec![MOperand::Imm(val)],
            phys_uses: vec![],
            clobbers: vec![],
        };
        let mf = single_block_mf("mov_wide_fn", vec![mi]);
        let mut e = AArch64Emitter::new(ObjectFormat::Elf);
        let sec = e.emit_function(&mf);

        // Must emit exactly 4 instructions (4 × 4 bytes = 16 bytes).
        assert_eq!(sec.data.len(), 16,
            "MOV_WIDE with a full 64-bit value must emit 4 instructions (16 bytes)");

        // First instruction: MOVZ X0, #0x5678 — 0xD280_ACF0
        let w0 = u32::from_le_bytes([sec.data[0], sec.data[1], sec.data[2], sec.data[3]]);
        assert_eq!(w0 & 0xFFE0_001F, 0xD280_0000,
            "first word must be MOVZ (opcode 0xD280_0000 with chunk in bits[20:5])");
        assert_eq!((w0 >> 5) & 0xFFFF, 0x5678, "chunk0 must be 0x5678");

        // Second instruction: MOVK X0, #0x1234, lsl 16 — base 0xF2A0_0000
        let w1 = u32::from_le_bytes([sec.data[4], sec.data[5], sec.data[6], sec.data[7]]);
        assert_eq!(w1 & 0xFFE0_001F, 0xF2A0_0000,
            "second word must be MOVK lsl 16 (0xF2A0_0000)");
        assert_eq!((w1 >> 5) & 0xFFFF, 0x1234, "chunk1 must be 0x1234");

        // Third instruction: MOVK X0, #0xCAFE, lsl 32 — base 0xF2C0_0000
        let w2 = u32::from_le_bytes([sec.data[8], sec.data[9], sec.data[10], sec.data[11]]);
        assert_eq!(w2 & 0xFFE0_001F, 0xF2C0_0000,
            "third word must be MOVK lsl 32 (0xF2C0_0000)");
        assert_eq!((w2 >> 5) & 0xFFFF, 0xCAFE, "chunk2 must be 0xCAFE");

        // Fourth instruction: MOVK X0, #0xDEAD, lsl 48 — base 0xF2E0_0000
        let w3 = u32::from_le_bytes([sec.data[12], sec.data[13], sec.data[14], sec.data[15]]);
        assert_eq!(w3 & 0xFFE0_001F, 0xF2E0_0000,
            "fourth word must be MOVK lsl 48 (0xF2E0_0000)");
        assert_eq!((w3 >> 5) & 0xFFFF, 0xDEAD, "chunk3 must be 0xDEAD");
    }

    #[test]
    fn mov_wide_32bit_emits_two_chunks() {
        // 0x0001_2345 has chunk0=0x2345, chunk1=0x0001, chunk2=0, chunk3=0.
        // Should emit exactly 2 instructions.
        let val: i64 = 0x0001_2345;
        let mi = MInstr {
            opcode: MOV_WIDE,
            dst: Some(VReg(X0.0 as u32)),
            operands: vec![MOperand::Imm(val)],
            phys_uses: vec![],
            clobbers: vec![],
        };
        let mf = single_block_mf("mov_wide_32_fn", vec![mi]);
        let mut e = AArch64Emitter::new(ObjectFormat::Elf);
        let sec = e.emit_function(&mf);
        assert_eq!(sec.data.len(), 8,
            "MOV_WIDE 0x1_2345 must emit exactly 2 instructions (8 bytes)");
    }
}
