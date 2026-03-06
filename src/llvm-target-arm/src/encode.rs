//! AArch64 machine-instruction encoding.
//!
//! Implements [`Emitter`] for AArch64, converting a [`MachineFunction`] into
//! a byte sequence of fixed-width 32-bit instruction words and producing
//! relocation records for unresolved branch targets and call destinations.
//!
//! Each AArch64 instruction is exactly 4 bytes.  Branches are patched in a
//! second pass once all block offsets are known.

use crate::{instructions::*, regs::reg_enc};
use llvm_codegen::{
    emit::{Emitter, ObjectFormat, Reloc, Section},
    isel::{MInstr, MOperand, MachineFunction, PReg},
};
use std::collections::HashMap;

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

        // Determine whether we need a frame (x29/x30 save + sub-sp).
        // Needed when there are spill slots OR when callee-saved regs were used.
        let n_cs = mf.used_callee_saved.len();
        let needs_frame = mf.frame_size > 0 || n_cs > 0;

        // Frame layout (x29 = new SP after pre-index):
        //   [x29 +  0] saved x29
        //   [x29 +  8] saved x30 (LR)
        //   [x29 + 16]                    ← callee-saved regs start
        //   [x29 + 16 + i*8]  saved X(19+i) for i in 0..n_cs
        //   [x29 + 16 + n_cs*8 + slot*8]  spill slots
        let cs_save_size = n_cs * 8;
        let frame_alloc = if needs_frame {
            ((16 + cs_save_size + mf.frame_size as usize) + 15) & !15
        } else {
            0
        };

        ctx.cs_save_count = n_cs as u32;

        // Emit prologue.
        if needs_frame {
            let frame_alloc_u32 = frame_alloc as u32;
            // stp x29, x30, [sp, #-frame_alloc]!
            // Encoding: 0xA9800000 | (imm7 << 15) | (x30=30 << 10) | (sp=31 << 5) | x29=29
            // imm7 = -frame_alloc/8 (signed 7-bit)
            let imm7 = ((-((frame_alloc_u32 / 8) as i32)) as u32) & 0x7F;
            ctx.emit4(0xA9800000 | (imm7 << 15) | (30 << 10) | (31 << 5) | 29);
            // add x29, sp, #0  (mov x29, sp)
            ctx.emit4(0x910003FD);
            // str Xreg, [x29, #(16 + i*8)] for each callee-saved register.
            // Unsigned-offset form: 0xF9000000 | (imm12 << 10) | (x29=29 << 5) | Rt
            for (i, &pr) in mf.used_callee_saved.iter().enumerate() {
                let rt = crate::regs::reg_enc(pr) as u32;
                let imm12 = (2 + i as u32) & 0xFFF; // (16 + i*8) / 8
                ctx.emit4(0xF9000000 | (imm12 << 10) | (29 << 5) | rt);
            }
        }

        // First pass: encode all instructions, recording branch patch sites.
        for (bi, block) in mf.blocks.iter().enumerate() {
            ctx.block_offsets.insert(bi, ctx.code.len());
            for instr in &block.instrs {
                // Emit epilogue before any RET when we have a frame.
                if instr.opcode == RET && needs_frame {
                    // ldr Xreg, [x29, #(16 + i*8)] for each callee-saved reg (reverse order).
                    for (i, &pr) in mf.used_callee_saved.iter().enumerate().rev() {
                        let rt = crate::regs::reg_enc(pr) as u32;
                        let imm12 = (2 + i as u32) & 0xFFF;
                        ctx.emit4(0xF9400000 | (imm12 << 10) | (29 << 5) | rt);
                    }
                    // ldp x29, x30, [sp], #frame_alloc (post-index)
                    // Encoding: 0xA8C00000 | (imm7 << 15) | (x30=30 << 10) | (sp=31 << 5) | x29=29
                    let frame_alloc_u32 = frame_alloc as u32;
                    let imm7 = ((frame_alloc_u32 / 8) as u32) & 0x7F;
                    ctx.emit4(0xA8C00000 | (imm7 << 15) | (30 << 10) | (31 << 5) | 29);
                }
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
            ObjectFormat::Elf => ".text",
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

    fn elf_machine(&self) -> u16 {
        183 // EM_AARCH64
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
    /// Number of callee-saved registers (X19–X28) saved in the prologue.
    /// Used by LDR_FP/STR_FP to compute the x29-relative slot offset:
    /// spill slot n lives at [x29 + 16 + cs_save_count*8 + n*8].
    cs_save_count: u32,
}

impl EncodeCtx {
    fn emit4(&mut self, word: u32) {
        self.code.extend_from_slice(&word.to_le_bytes());
    }
    fn pos(&self) -> usize {
        self.code.len()
    }
}

// ── instruction encoding ─────────────────────────────────────────────────

fn encode_instr(instr: &MInstr, ctx: &mut EncodeCtx) {
    // Helper to extract PReg from operand.
    let preg = |op: &MOperand| -> Option<PReg> {
        match op {
            MOperand::PReg(r) => Some(*r),
            _ => None,
        }
    };
    let imm = |op: &MOperand| -> Option<i64> {
        match op {
            MOperand::Imm(v) => Some(*v),
            _ => None,
        }
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
                let chunk0 = ((val_u64) & 0xFFFF) as u32;
                let chunk1 = ((val_u64 >> 16) & 0xFFFF) as u32;
                let chunk2 = ((val_u64 >> 32) & 0xFFFF) as u32;
                let chunk3 = ((val_u64 >> 48) & 0xFFFF) as u32;
                // Always emit MOVZ for chunk0 (clears the register).
                ctx.emit4(0xD2800000 | (chunk0 << 5) | rd);
                if chunk1 != 0 {
                    ctx.emit4(0xF2A00000 | (chunk1 << 5) | rd);
                }
                if chunk2 != 0 {
                    ctx.emit4(0xF2C00000 | (chunk2 << 5) | rd);
                }
                if chunk3 != 0 {
                    ctx.emit4(0xF2E00000 | (chunk3 << 5) | rd);
                }
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
                // CSINC Rd, XZR, XZR, inv_cond.
                // Base constant: sf=1, op=0, S=0, bits[28:21]=11010110, Rm=XZR(31),
                // cond=0000, o2=0, o1=1, Rn=XZR(31), Rd=0 → 0x9ADF07E0.
                ctx.emit4(0x9ADF07E0 | ((inv_cond as u32) << 12) | rd);
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
            if let (Some(MOperand::Imm(cc)), Some(MOperand::Block(target))) =
                (instr.operands.first(), instr.operands.get(1))
            {
                let hw_cond = cc_to_hw(*cc);
                let patch_pos = ctx.pos();
                ctx.emit4(0x54000000 | hw_cond as u32); // placeholder
                ctx.branch_patches.push((patch_pos, *target));
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

        // ── LDR / STR (generic: placeholder NOP) ─────────────────────────
        LDR | STR => {
            ctx.emit4(0xD503201F);
        }

        // ── LDR_FP: ldr xd, [x29, #(16 + cs_save_count*8 + slot*8)] ────
        // Unsigned offset form: 0xF9400000 | (imm12 << 10) | (Rn << 5) | Rt
        // imm12 = (16 + cs_save_count*8 + slot*8) / 8 = 2 + cs_save_count + slot.
        LDR_FP => {
            if let (Some(dst), Some(MOperand::Imm(slot))) = (instr.dst, instr.operands.first()) {
                let rd = reg_enc(PReg(dst.0 as u8)) as u32;
                let imm12 = (2 + ctx.cs_save_count + *slot as u32) & 0xFFF;
                ctx.emit4(0xF9400000 | (imm12 << 10) | (29 << 5) | rd);
            } else {
                ctx.emit4(0xD503201F);
            }
        }

        // ── STR_FP: str xs, [x29, #(16 + cs_save_count*8 + slot*8)] ────
        // Unsigned offset form: 0xF9000000 | (imm12 << 10) | (Rn << 5) | Rt
        STR_FP => {
            if let (Some(MOperand::Imm(slot)), Some(src)) =
                (instr.operands.first(), instr.operands.get(1).and_then(preg))
            {
                let rt = reg_enc(src) as u32;
                let imm12 = (2 + ctx.cs_save_count + *slot as u32) & 0xFFF;
                ctx.emit4(0xF9000000 | (imm12 << 10) | (29 << 5) | rt);
            } else {
                ctx.emit4(0xD503201F);
            }
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
            MOperand::PReg(r) => Some(*r),
            _ => None,
        }),
        instr.operands.get(1).and_then(|op| match op {
            MOperand::PReg(r) => Some(*r),
            _ => None,
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
        if let MOperand::PReg(r) = op {
            Some(*r)
        } else {
            None
        }
    });
    (dst, src)
}

/// Extract two PReg operands from `instr.operands`.
fn get_two_pregs(instr: &MInstr) -> (Option<PReg>, Option<PReg>) {
    let mut it = instr.operands.iter().filter_map(|op| {
        if let MOperand::PReg(r) = op {
            Some(*r)
        } else {
            None
        }
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
        CC_EQ => 0,  // EQ (Z=1)
        CC_NE => 1,  // NE (Z=0)
        CC_LT => 11, // LT (N!=V)
        CC_LE => 13, // LE (Z=1 or N!=V)
        CC_GT => 12, // GT (Z=0 and N=V)
        CC_GE => 10, // GE (N=V)
        CC_LO => 3,  // LO/CC (C=0)
        CC_LS => 9,  // LS (C=0 or Z=1)
        CC_HI => 8,  // HI (C=1 and Z=0)
        CC_HS => 2,  // HS/CS (C=1)
        _ => 0,
    }
}

// ── tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::regs::{X0, X1, X2};
    use llvm_codegen::{
        emit::emit_object,
        isel::{MInstr, MachineFunction, VReg},
    };

    fn single_block_mf(name: &str, instrs: Vec<MInstr>) -> MachineFunction {
        let mut mf = MachineFunction::new(name.into());
        let b = mf.add_block("entry");
        for i in instrs {
            mf.push(b, i);
        }
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
        assert_eq!(
            &sec.data[0..4],
            &[0xC0, 0x03, 0x5F, 0xD6],
            "RET must encode as 0xD65F03C0"
        );
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
        assert_eq!(
            word, 0x8B020020,
            "add x0, x1, x2 should encode as 0x8B020020"
        );
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
        assert_eq!(
            b_word & 0xFC000000,
            0x14000000,
            "unconditional branch base bits"
        );
        assert_eq!(
            b_word & 0x03FFFFFF,
            1,
            "branch offset should be 1 instruction forward"
        );
    }

    #[test]
    fn elf_object_has_aarch64_machine_type() {
        let mf = single_block_mf("fn1", vec![MInstr::new(RET)]);
        let mut e = AArch64Emitter::new(ObjectFormat::Elf);
        let obj = emit_object(&mf, &mut e);
        let bytes = obj.to_bytes();
        // ELF header: e_machine is at bytes [18..20], EM_AARCH64 = 183 = 0xB7
        let e_machine = u16::from_le_bytes([bytes[18], bytes[19]]);
        assert_eq!(&bytes[0..4], b"\x7fELF", "ELF magic must be present");
        assert_eq!(e_machine, 183, "EM_AARCH64");
    }

    #[test]
    fn macho_object_contains_ret() {
        let mf = single_block_mf("fn2", vec![MInstr::new(RET)]);
        let mut e = AArch64Emitter::new(ObjectFormat::MachO);
        let sec = e.emit_function(&mf);
        // RET = 0xD65F03C0; byte 0 = 0xC0
        assert!(
            sec.data.contains(&0xC0),
            "RET byte 0 (0xC0) must be in code"
        );
    }

    #[test]
    fn ldr_fp_slot0_encodes_correctly() {
        // LDR_FP: ldr x0, [x29, #16]  (slot 0 → imm12 = 2+0 = 2)
        // Encoding: 0xF9400000 | (2 << 10) | (29 << 5) | 0
        //         = 0xF9400000 | 0x800 | 0x3A0 | 0 = 0xF9400BA0
        use crate::instructions::LDR_FP;
        let mi = MInstr {
            opcode: LDR_FP,
            dst: Some(VReg(X0.0 as u32)),
            operands: vec![MOperand::Imm(0)],
            phys_uses: vec![],
            clobbers: vec![],
        };
        let mf = single_block_mf("ldr_fp_fn", vec![mi]);
        let mut e = AArch64Emitter::new(ObjectFormat::Elf);
        let sec = e.emit_function(&mf);
        let word = u32::from_le_bytes([sec.data[0], sec.data[1], sec.data[2], sec.data[3]]);
        // 0xF9400000 | (2 << 10) | (29 << 5) | 0
        let expected = 0xF9400000u32 | (2 << 10) | (29 << 5);
        assert_eq!(word, expected, "ldr x0, [x29, #16] encoding");
    }

    #[test]
    fn str_fp_slot0_encodes_correctly() {
        // STR_FP: str x1, [x29, #16]  (slot 0 → imm12 = 2)
        // Encoding: 0xF9000000 | (2 << 10) | (29 << 5) | 1
        use crate::instructions::STR_FP;
        let mi = MInstr {
            opcode: STR_FP,
            dst: None,
            operands: vec![MOperand::Imm(0), MOperand::PReg(X1)],
            phys_uses: vec![],
            clobbers: vec![],
        };
        let mf = single_block_mf("str_fp_fn", vec![mi]);
        let mut e = AArch64Emitter::new(ObjectFormat::Elf);
        let sec = e.emit_function(&mf);
        let word = u32::from_le_bytes([sec.data[0], sec.data[1], sec.data[2], sec.data[3]]);
        let expected = 0xF9000000u32 | (2 << 10) | (29 << 5) | 1;
        assert_eq!(word, expected, "str x1, [x29, #16] encoding");
    }

    #[test]
    fn prologue_emitted_when_frame_size_nonzero() {
        // frame_size=8 → N=(16+8+15)&!15=32 → imm7=-4 → stp x29,x30,[sp,#-32]!
        // stp encoding: 0xA9800000 | (imm7<<15) | (30<<10) | (31<<5) | 29
        // imm7 = (-32/8) as u32 & 0x7F = (-4i32 as u32) & 0x7F = 0x7C
        // = 0xA9800000 | (0x7C<<15) | (30<<10) | (31<<5) | 29
        // = 0xA9800000 | 0x3E00000 | 0x7800 | 0x3E0 | 0x1D = 0xABBE7BFD
        // (let's verify the bytes instead of the whole word)
        let mut mf = MachineFunction::new("framed_fn".into());
        mf.frame_size = 8; // 1 spill slot
        let b = mf.add_block("entry");
        mf.push(b, MInstr::new(RET));
        let mut e = AArch64Emitter::new(ObjectFormat::Elf);
        let sec = e.emit_function(&mf);

        // Must have at least prologue (4B stp + 4B add) + epilogue (4B ldp) + ret (4B) = 16 bytes.
        assert!(sec.data.len() >= 16, "must have prologue+epilogue+ret");

        // First instruction: stp x29, x30, [sp, #-N]! — high byte should indicate STP pre-index.
        let w0 = u32::from_le_bytes([sec.data[0], sec.data[1], sec.data[2], sec.data[3]]);
        // STP pre-index 64-bit: bits[31:23] = 101010011 = 0xA9...(high bits), bit[22]=0(store)
        assert_eq!(
            w0 & 0xFFC00000,
            0xA9800000,
            "first word should be STP pre-index store (0xA98xxxxx)"
        );

        // Second instruction: add x29, sp, #0 = 0x910003FD
        let w1 = u32::from_le_bytes([sec.data[4], sec.data[5], sec.data[6], sec.data[7]]);
        assert_eq!(w1, 0x910003FD, "second word must be add x29, sp, #0");

        // Epilogue before RET: ldp x29, x30, [sp], #N — bits[31:23] indicate LDP post-index.
        let epilogue_off = sec.data.len() - 8; // ldp is 4B before ret 4B
        let w_ldp = u32::from_le_bytes([
            sec.data[epilogue_off],
            sec.data[epilogue_off + 1],
            sec.data[epilogue_off + 2],
            sec.data[epilogue_off + 3],
        ]);
        // LDP post-index 64-bit load: bits[31:22] = 1010100011 = 0xA8C
        assert_eq!(
            w_ldp & 0xFFC00000,
            0xA8C00000,
            "epilogue should be LDP post-index load (0xA8Cxxxxx)"
        );

        // Last instruction: RET = 0xD65F03C0
        let w_ret = u32::from_le_bytes([
            sec.data[sec.data.len() - 4],
            sec.data[sec.data.len() - 3],
            sec.data[sec.data.len() - 2],
            sec.data[sec.data.len() - 1],
        ]);
        assert_eq!(w_ret, 0xD65F03C0, "last instruction must be RET");
    }

    #[test]
    fn no_prologue_when_frame_size_zero() {
        // frame_size=0 → no prologue/epilogue → just RET.
        let mf = single_block_mf("plain_fn", vec![MInstr::new(RET)]);
        let mut e = AArch64Emitter::new(ObjectFormat::Elf);
        let sec = e.emit_function(&mf);
        assert_eq!(sec.data.len(), 4, "only RET (4 bytes), no prologue");
        let w = u32::from_le_bytes([sec.data[0], sec.data[1], sec.data[2], sec.data[3]]);
        assert_eq!(w, 0xD65F03C0, "must be RET");
    }

    #[test]
    fn spill_end_to_end_aarch64() {
        use crate::instructions::{LDR_FP, STR_FP};
        use crate::regs::X0;
        use llvm_codegen::isel::MOpcode;
        use llvm_codegen::regalloc::{allocate_registers, apply_allocation, compute_live_intervals, insert_spill_reloads, RegAllocStrategy};

        let mut mf = MachineFunction::new("spill_e2e_arm".into());
        // Only 1 allocatable register → forces a spill.
        mf.allocatable_pregs = vec![X0];
        mf.callee_saved_pregs = vec![];
        let b = mf.add_block("entry");
        let v0 = mf.fresh_vreg();
        mf.push(b, MInstr::new(MOpcode(0x10)).with_dst(v0));
        let v1 = mf.fresh_vreg();
        mf.push(b, MInstr::new(MOpcode(0x10)).with_dst(v1).with_vreg(v0));
        mf.push(b, MInstr::new(RET));

        let intervals = compute_live_intervals(&mf);
        let mut result = allocate_registers(&intervals, &mf.allocatable_pregs, RegAllocStrategy::LinearScan);
        assert!(!result.spilled.is_empty(), "must have spills");
        insert_spill_reloads(&mut mf, &mut result, LDR_FP, STR_FP);
        apply_allocation(&mut mf, &result);

        let mut e = AArch64Emitter::new(ObjectFormat::Elf);
        let sec = e.emit_function(&mf);

        assert!(!sec.data.is_empty(), "emitted code must be non-empty");
        // Prologue must be present (frame_size > 0): first word is STP.
        let w0 = u32::from_le_bytes([sec.data[0], sec.data[1], sec.data[2], sec.data[3]]);
        assert_eq!(
            w0 & 0xFFC00000,
            0xA9800000,
            "prologue STP must be first instruction"
        );
        // RET = 0xD65F03C0 must be present somewhere.
        let found_ret = sec
            .data
            .chunks(4)
            .any(|c| c.len() == 4 && u32::from_le_bytes([c[0], c[1], c[2], c[3]]) == 0xD65F03C0);
        assert!(found_ret, "RET must be present in emitted code");
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
        assert_eq!(
            sec.data.len(),
            16,
            "MOV_WIDE with a full 64-bit value must emit 4 instructions (16 bytes)"
        );

        // First instruction: MOVZ X0, #0x5678 — 0xD280_ACF0
        let w0 = u32::from_le_bytes([sec.data[0], sec.data[1], sec.data[2], sec.data[3]]);
        assert_eq!(
            w0 & 0xFFE0_001F,
            0xD280_0000,
            "first word must be MOVZ (opcode 0xD280_0000 with chunk in bits[20:5])"
        );
        assert_eq!((w0 >> 5) & 0xFFFF, 0x5678, "chunk0 must be 0x5678");

        // Second instruction: MOVK X0, #0x1234, lsl 16 — base 0xF2A0_0000
        let w1 = u32::from_le_bytes([sec.data[4], sec.data[5], sec.data[6], sec.data[7]]);
        assert_eq!(
            w1 & 0xFFE0_001F,
            0xF2A0_0000,
            "second word must be MOVK lsl 16 (0xF2A0_0000)"
        );
        assert_eq!((w1 >> 5) & 0xFFFF, 0x1234, "chunk1 must be 0x1234");

        // Third instruction: MOVK X0, #0xCAFE, lsl 32 — base 0xF2C0_0000
        let w2 = u32::from_le_bytes([sec.data[8], sec.data[9], sec.data[10], sec.data[11]]);
        assert_eq!(
            w2 & 0xFFE0_001F,
            0xF2C0_0000,
            "third word must be MOVK lsl 32 (0xF2C0_0000)"
        );
        assert_eq!((w2 >> 5) & 0xFFFF, 0xCAFE, "chunk2 must be 0xCAFE");

        // Fourth instruction: MOVK X0, #0xDEAD, lsl 48 — base 0xF2E0_0000
        let w3 = u32::from_le_bytes([sec.data[12], sec.data[13], sec.data[14], sec.data[15]]);
        assert_eq!(
            w3 & 0xFFE0_001F,
            0xF2E0_0000,
            "fourth word must be MOVK lsl 48 (0xF2E0_0000)"
        );
        assert_eq!((w3 >> 5) & 0xFFFF, 0xDEAD, "chunk3 must be 0xDEAD");
    }

    // ── issue #73: CSINC (CSET) encoding ─────────────────────────────────

    #[test]
    fn cset_eq_encodes_correctly() {
        // CSET X0, EQ  →  CSINC X0, XZR, XZR, NE  (inv_cond = EQ^1 = NE = 1)
        // Base: 0x9ADF07E0; cond field at bits[15:12]; rd at bits[4:0].
        // inv_cond = 1 (NE), Rd = X0 = 0
        // → 0x9ADF07E0 | (1 << 12) | 0 = 0x9ADF17E0
        use crate::instructions::CSET;
        let mi = MInstr {
            opcode: CSET,
            dst: Some(VReg(X0.0 as u32)),
            operands: vec![MOperand::Imm(CC_EQ)],
            phys_uses: vec![],
            clobbers: vec![],
        };
        let mf = single_block_mf("cset_fn", vec![mi]);
        let mut e = AArch64Emitter::new(ObjectFormat::Elf);
        let sec = e.emit_function(&mf);
        let word = u32::from_le_bytes([sec.data[0], sec.data[1], sec.data[2], sec.data[3]]);
        // CSINC X0, XZR, XZR, NE: bits[31:21]=10011010110, Rm=31, cond=0001, o2o1=01, Rn=31, Rd=0
        // = 0x9ADF07E0 | (1 << 12) = 0x9ADF17E0
        assert_eq!(
            word, 0x9ADF17E0,
            "CSET X0, EQ must encode as CSINC X0, XZR, XZR, NE = 0x9ADF17E0"
        );
        // Verify the old wrong constant is NOT present (sanity check against regression).
        assert_ne!(
            word & 0xFFFF0000,
            0x9A9F0000,
            "old wrong base 0x9A9F07E0 must not be used"
        );
    }

    #[test]
    fn cset_lt_encodes_correctly() {
        // CSET X1, LT  →  CSINC X1, XZR, XZR, GE  (inv_cond = LT^1 = GE = 10^1 = 11 = 11)
        // cc_to_hw(CC_LT) = 11 (LT), inv_cond = 11^1 = 10 (GE)
        // Rd = X1 = reg_enc(X1) = 1
        // → 0x9ADF07E0 | (10 << 12) | 1 = 0x9ADFA7E1
        use crate::instructions::CSET;
        let mi = MInstr {
            opcode: CSET,
            dst: Some(VReg(X1.0 as u32)),
            operands: vec![MOperand::Imm(CC_LT)],
            phys_uses: vec![],
            clobbers: vec![],
        };
        let mf = single_block_mf("cset_lt_fn", vec![mi]);
        let mut e = AArch64Emitter::new(ObjectFormat::Elf);
        let sec = e.emit_function(&mf);
        let word = u32::from_le_bytes([sec.data[0], sec.data[1], sec.data[2], sec.data[3]]);
        // hw_cond(LT) = 11, inv = 10, Rd = 1
        // = 0x9ADF07E0 | (10 << 12) | 1 = 0x9ADFA7E1
        assert_eq!(
            word, 0x9ADFA7E1,
            "CSET X1, LT must encode as CSINC X1, XZR, XZR, GE = 0x9ADFA7E1"
        );
    }

    // ── issue #74: AArch64 callee-saved register save/restore ────────────

    #[test]
    fn prologue_saves_callee_saved_regs() {
        // A function that uses X19 (callee-saved) should emit:
        //   stp x29, x30, [sp, #-N]!
        //   add x29, sp, #0
        //   str x19, [x29, #16]          ← callee-saved save
        //   ...
        //   ldr x19, [x29, #16]          ← callee-saved restore
        //   ldp x29, x30, [sp], #N
        //   ret
        use crate::regs::*;
        let mut mf = MachineFunction::new("cs_fn".into());
        mf.used_callee_saved = vec![X19];
        let b = mf.add_block("entry");
        mf.push(b, MInstr::new(RET));

        let mut e = AArch64Emitter::new(ObjectFormat::Elf);
        let sec = e.emit_function(&mf);

        // Layout: stp(4) + add(4) + str_x19(4) + ldr_x19(4) + ldp(4) + ret(4) = 24 bytes
        assert_eq!(sec.data.len(), 24, "must have prologue + cs saves + epilogue + ret");

        // Word 0: STP pre-index
        let w0 = u32::from_le_bytes([sec.data[0], sec.data[1], sec.data[2], sec.data[3]]);
        assert_eq!(w0 & 0xFFC00000, 0xA9800000, "word 0 must be STP pre-index");

        // Word 1: add x29, sp, #0 = 0x910003FD
        let w1 = u32::from_le_bytes([sec.data[4], sec.data[5], sec.data[6], sec.data[7]]);
        assert_eq!(w1, 0x910003FD, "word 1 must be add x29, sp, #0");

        // Word 2: str x19, [x29, #16]
        // STR unsigned-offset: 0xF9000000 | (imm12=2 << 10) | (29 << 5) | reg_enc(X19)
        // X19 = PReg(19), reg_enc = 19 & 0x1F = 19
        let w2 = u32::from_le_bytes([sec.data[8], sec.data[9], sec.data[10], sec.data[11]]);
        let expected_str = 0xF9000000u32 | (2 << 10) | (29 << 5) | 19;
        assert_eq!(w2, expected_str, "word 2 must be str x19, [x29, #16]");

        // Word 3 (epilogue): ldr x19, [x29, #16]
        let w3 = u32::from_le_bytes([sec.data[12], sec.data[13], sec.data[14], sec.data[15]]);
        let expected_ldr = 0xF9400000u32 | (2 << 10) | (29 << 5) | 19;
        assert_eq!(w3, expected_ldr, "word 3 must be ldr x19, [x29, #16]");

        // Word 4: LDP post-index
        let w4 = u32::from_le_bytes([sec.data[16], sec.data[17], sec.data[18], sec.data[19]]);
        assert_eq!(w4 & 0xFFC00000, 0xA8C00000, "word 4 must be LDP post-index");

        // Word 5: RET
        let w5 = u32::from_le_bytes([sec.data[20], sec.data[21], sec.data[22], sec.data[23]]);
        assert_eq!(w5, 0xD65F03C0, "word 5 must be RET");
    }

    #[test]
    fn needs_frame_when_callee_saved_but_no_spills() {
        // used_callee_saved non-empty but frame_size == 0 — must still emit prologue.
        use crate::regs::X20;
        let mut mf = MachineFunction::new("cs_noframe".into());
        mf.used_callee_saved = vec![X20];
        // frame_size is 0 (no spill slots)
        let b = mf.add_block("entry");
        mf.push(b, MInstr::new(RET));

        let mut e = AArch64Emitter::new(ObjectFormat::Elf);
        let sec = e.emit_function(&mf);

        // Must have more than just RET (prologue must be present).
        assert!(sec.data.len() > 4, "must emit prologue even with no spill slots");
        let w0 = u32::from_le_bytes([sec.data[0], sec.data[1], sec.data[2], sec.data[3]]);
        assert_eq!(
            w0 & 0xFFC00000,
            0xA9800000,
            "first word must be STP (prologue) even when frame_size == 0"
        );
    }

    #[test]
    fn spill_slot_offset_accounts_for_callee_saved() {
        // With 1 callee-saved reg, spill slot 0 should be at [x29 + 24] (imm12 = 3),
        // not [x29 + 16] (imm12 = 2).
        use crate::instructions::LDR_FP;
        use crate::regs::X19;
        let mut mf = MachineFunction::new("cs_slot_fn".into());
        mf.used_callee_saved = vec![X19];
        mf.frame_size = 8; // 1 spill slot
        let b = mf.add_block("entry");
        let v = mf.fresh_vreg();
        mf.push(
            b,
            MInstr {
                opcode: LDR_FP,
                dst: Some(v),
                operands: vec![MOperand::Imm(0)], // slot 0
                phys_uses: vec![],
                clobbers: vec![],
            },
        );
        mf.push(b, MInstr::new(RET));

        let mut e = AArch64Emitter::new(ObjectFormat::Elf);
        let sec = e.emit_function(&mf);

        // Find the LDR_FP instruction.  Layout:
        //   stp(4) + add(4) + str_x19(4) + ldr_fp(4) + ldr_x19(4) + ldp(4) + ret(4) = 28B
        // LDR_FP is at byte offset 12.
        let ldr_word =
            u32::from_le_bytes([sec.data[12], sec.data[13], sec.data[14], sec.data[15]]);
        // imm12 should be 3 (= 2 + cs_save_count=1 + slot=0)
        // ldr x_reg, [x29, #24]: 0xF9400000 | (3 << 10) | (29 << 5) | rd
        let imm12_actual = (ldr_word >> 10) & 0xFFF;
        assert_eq!(
            imm12_actual, 3,
            "with 1 callee-saved reg, slot 0 should be at imm12=3 ([x29, #24]), not imm12=2"
        );
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
        assert_eq!(
            sec.data.len(),
            8,
            "MOV_WIDE 0x1_2345 must emit exactly 2 instructions (8 bytes)"
        );
    }
}
