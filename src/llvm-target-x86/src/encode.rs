//! x86_64 machine-instruction encoding.
//!
//! Implements `Emitter` for x86_64, converting a `MachineFunction` into
//! a byte sequence and producing relocation records for unresolved branch
//! targets and call destinations.
//!
//! Only the most common 64-bit integer instructions are encoded; unsupported
//! opcodes fall through to a single `NOP` (`0x90`) so the output is always
//! syntactically valid machine code.

use crate::{
    instructions::*,
    regs::{is_extended, is_fp_reg, reg_enc, xmm_enc},
};
use llvm_codegen::{
    cfi::{CfiInstr, CfiWriter},
    emit::{DebugLineRow, Emitter, ObjectFormat, Reloc, RelocKind, Section},
    isel::{MInstr, MOperand, MachineFunction, PReg},
};
use std::collections::HashMap;

/// x86_64 code emitter.
///
/// Implements [`Emitter`] for x86-64, converting a [`MachineFunction`] into
/// a byte sequence and producing relocation records for unresolved branch
/// targets and call destinations.
///
/// After calling [`emit_function`], the structured CFI prologue for the last
/// emitted function is available via [`take_cfi`].  This can be used by
/// higher-level drivers that want to produce `.eh_frame` sections with
/// accurate per-function prologue instructions.
///
/// [`emit_function`]: Emitter::emit_function
/// [`take_cfi`]: X86Emitter::take_cfi
pub struct X86Emitter {
    /// Object format (ELF / Mach-O / COFF).
    pub format: ObjectFormat,
    /// Structured CFI section assembled after the last [`emit_function`] call.
    ///
    /// Contains a fully-assembled `.eh_frame` blob (one CIE + one FDE) built
    /// from the machine function's prologue shape using [`CfiWriter`].
    /// `None` if no function has been encoded yet.
    ///
    /// [`emit_function`]: Emitter::emit_function
    cfi_section: Option<Vec<u8>>,
}

impl X86Emitter {
    /// Create a new `X86Emitter` for the given object format.
    pub fn new(format: ObjectFormat) -> Self {
        Self { format, cfi_section: None }
    }

    /// Return and clear the CFI section bytes built during the last
    /// [`emit_function`] call, or `None` if no function has been emitted.
    ///
    /// The returned bytes form a complete `.eh_frame` section (CIE + FDE)
    /// for the most recently encoded function.
    ///
    /// [`emit_function`]: Emitter::emit_function
    pub fn take_cfi(&mut self) -> Option<Vec<u8>> {
        self.cfi_section.take()
    }
}

impl Emitter for X86Emitter {
    fn emit_function(&mut self, mf: &MachineFunction) -> Section {
        let mut ctx = EncodeCtx::default();
        let mut debug_rows: Vec<DebugLineRow> = Vec::new();

        let n_callee = mf.used_callee_saved.len();
        let needs_frame = mf.frame_size > 0 || n_callee > 0 || mf.alloca_frame_bytes > 0;

        // Build the structured CFI prologue instructions.
        // These are recorded here (before encoding) so that byte offsets match
        // the actual prologue layout: push rbp (1 byte), mov rbp rsp (3 bytes).
        let prologue_cfi: Vec<CfiInstr> = if needs_frame {
            vec![
                // After "push rbp" (1 byte): CFA = RSP+16, RBP saved at CFA-16.
                CfiInstr::AdvanceLoc(1),
                CfiInstr::DefCfaOffset(16),
                CfiInstr::Offset { reg: 6, factored_offset: 2 },
                // After "mov rbp, rsp" (3 bytes): CFA register = RBP.
                CfiInstr::AdvanceLoc(3),
                CfiInstr::DefCfaRegister(6),
            ]
        } else {
            vec![]
        };

        // Compute the `sub rsp` amount so that RSP is 16-byte aligned after the
        // prologue.  After `push rbp` (1 push) and n_callee additional pushes,
        // the stack has moved by (1 + n_callee) * 8 bytes from the entry RSP
        // (which is 8 mod 16 because the call already pushed the return address).
        // We need: (1 + n_callee) * 8 + sub_rsp ≡ 0 (mod 16).
        // `raw` includes both spill slots and alloca frame slots.
        let sub_rsp: usize = if needs_frame {
            let needs_align8 = n_callee % 2 == 1; // odd n_callee → need sub_rsp ≡ 8 (mod 16)
            let raw = (mf.frame_size + mf.alloca_frame_bytes) as usize;
            if needs_align8 {
                let r = raw % 16;
                match r.cmp(&8) {
                    std::cmp::Ordering::Equal => raw,
                    std::cmp::Ordering::Less => raw + (8 - r),
                    std::cmp::Ordering::Greater => raw + (24 - r),
                }
            } else {
                (raw + 15) & !15
            }
        } else {
            0
        };

        ctx.callee_save_bytes = (n_callee * 8) as u32;
        ctx.alloca_frame_bytes = mf.alloca_frame_bytes;

        // Emit prologue.
        if needs_frame {
            // push rbp  (0x55)
            ctx.emit(0x55);
            // mov rbp, rsp  (REX.W 0x89 ModRM: mod=11, reg=RSP(4), rm=RBP(5) → 0xE5)
            ctx.emit(0x48);
            ctx.emit(0x89);
            ctx.emit(0xE5);
            // push callee-saved registers.
            for &pr in &mf.used_callee_saved {
                if is_extended(pr) {
                    ctx.emit(0x41);
                }
                ctx.emit(0x50 | reg_enc(pr));
            }
            // sub rsp, sub_rsp  (or __chkstk sequence for large frames)
            if sub_rsp > 0 {
                if sub_rsp > 4096 && self.format == ObjectFormat::Coff {
                    // Win64 ABI requires __chkstk for frames > 4096 bytes to
                    // probe all pages of the allocation (prevents stack overflow
                    // from jumping over the guard page).
                    //
                    // Sequence: mov rax, sub_rsp  (REX.W B8 + imm64, 10 bytes)
                    //           call __chkstk      (E8 + rel32, 5 bytes)
                    //           sub rsp, rax       (REX.W 2B E0, 3 bytes)
                    ctx.emit(0x48); // REX.W
                    ctx.emit(0xB8); // MOV RAX, imm64
                    ctx.emit64(sub_rsp as i64);
                    let call_site = ctx.pos();
                    ctx.emit(0xE8); // CALL rel32
                    // rel32 placeholder — filled by linker via IMAGE_REL_AMD64_REL32
                    ctx.emit32(0);
                    ctx.relocs.push(Reloc {
                        offset: (call_site + 1) as u64,
                        symbol: 0, // resolved by emit_object via external_name
                        kind: RelocKind::Pc32,
                        addend: -4,
                        external_name: Some("__chkstk".to_string()),
                    });
                    // SUB RSP, RAX: REX.W=48, opcode=2B (SUB r64,r/m64),
                    // ModRM=E0 (mod=11, reg=RSP(4), rm=RAX(0) → 11_100_000)
                    ctx.emit(0x48);
                    ctx.emit(0x2B);
                    ctx.emit(0xE0);
                } else if sub_rsp <= 127 {
                    ctx.emit(0x48);
                    ctx.emit(0x83);
                    ctx.emit(0xEC);
                    ctx.emit(sub_rsp as u8);
                } else {
                    ctx.emit(0x48);
                    ctx.emit(0x81);
                    ctx.emit(0xEC);
                    ctx.code.extend_from_slice(&(sub_rsp as u32).to_le_bytes());
                }
            }
        }

        // First pass: encode all instructions, patching branches later.
        for (bi, block) in mf.blocks.iter().enumerate() {
            ctx.block_offsets.insert(bi, ctx.code.len());
            for instr in &block.instrs {
                let instr_addr = ctx.code.len() as u64;
                // Emit epilogue before any RET instruction when we have a frame.
                if instr.opcode == RET && needs_frame {
                    // add rsp, sub_rsp
                    if sub_rsp > 0 {
                        if sub_rsp <= 127 {
                            ctx.emit(0x48);
                            ctx.emit(0x83);
                            ctx.emit(0xC4);
                            ctx.emit(sub_rsp as u8);
                        } else {
                            ctx.emit(0x48);
                            ctx.emit(0x81);
                            ctx.emit(0xC4);
                            ctx.code.extend_from_slice(&(sub_rsp as u32).to_le_bytes());
                        }
                    }
                    // pop callee-saved (reverse order)
                    for &pr in mf.used_callee_saved.iter().rev() {
                        if is_extended(pr) {
                            ctx.emit(0x41);
                        }
                        ctx.emit(0x58 | reg_enc(pr));
                    }
                    // pop rbp  (0x5D)
                    ctx.emit(0x5D);
                }
                encode_instr(instr, &mut ctx);
                if let Some(loc) = instr.debug_loc {
                    debug_rows.push(DebugLineRow {
                        address: instr_addr,
                        line: loc.line,
                        column: loc.column,
                    });
                }
            }
        }

        // Second pass: patch near branches.
        for (patch_off, target_block) in ctx.branch_patches {
            if let Some(&target_off) = ctx.block_offsets.get(&target_block) {
                // rel32 = target - (patch_off + 4)
                let rel = (target_off as i64) - (patch_off as i64 + 4);
                let bytes = (rel as i32).to_le_bytes();
                ctx.code[patch_off..patch_off + 4].copy_from_slice(&bytes);
            }
        }

        let func_size = ctx.code.len() as u32;

        // Assemble the structured .eh_frame section using CfiWriter.
        // This produces one CIE + one FDE with the prologue CFI computed above.
        let mut cfi = CfiWriter::new();
        cfi.write_cie();
        cfi.write_fde(func_size.max(1), &prologue_cfi);
        self.cfi_section = Some(cfi.assemble());

        let section_name = match self.format {
            ObjectFormat::Elf => ".text",
            ObjectFormat::MachO => "__text",
            ObjectFormat::Coff => ".text",
        };

        Section {
            name: section_name.into(),
            data: ctx.code,
            relocs: ctx.relocs,
            debug_rows,
        }
    }

    fn object_format(&self) -> ObjectFormat {
        self.format
    }

    fn elf_machine(&self) -> u16 {
        62 // EM_X86_64
    }
}

// ── encoding context ──────────────────────────────────────────────────────

#[derive(Default)]
struct EncodeCtx {
    code: Vec<u8>,
    /// branch_patches: (byte_offset_of_rel32, target_block_index)
    branch_patches: Vec<(usize, usize)>,
    block_offsets: HashMap<usize, usize>,
    relocs: Vec<Reloc>,
    /// Bytes below RBP consumed by callee-saved pushes (n_callee * 8).
    /// Used to compute the correct RBP-relative displacement for spill and alloca slots.
    callee_save_bytes: u32,
    /// Bytes reserved for non-promotable alloca slots, placed directly below the
    /// callee-saved area.  Spill slots are placed below the alloca area, so their
    /// displacement formula includes this extra offset.
    alloca_frame_bytes: u32,
}

impl EncodeCtx {
    fn emit(&mut self, b: u8) {
        self.code.push(b);
    }
    fn emit32(&mut self, v: i32) {
        self.code.extend_from_slice(&v.to_le_bytes());
    }
    fn emit64(&mut self, v: i64) {
        self.code.extend_from_slice(&v.to_le_bytes());
    }
    fn pos(&self) -> usize {
        self.code.len()
    }
}

// ── REX prefix helpers ───────────────────────────────────────────────────

/// Emit a REX prefix only if needed (extended registers or explicit 64-bit).
fn maybe_rex(ctx: &mut EncodeCtx, w: bool, r: PReg, rm: PReg) {
    let r_ext = is_extended(r);
    let b_ext = is_extended(rm);
    if w || r_ext || b_ext {
        ctx.emit(
            0x40 | (if w { 0x08 } else { 0 })
                | (if r_ext { 0x04 } else { 0 })
                | (if b_ext { 0x01 } else { 0 }),
        );
    }
}

/// ModRM byte: mod=11 (register), reg field = r, rm field = rm.
fn modrm_rr(r: PReg, rm: PReg) -> u8 {
    0xC0 | (reg_enc(r) << 3) | reg_enc(rm)
}

/// ModRM byte for [ptr_reg] addressing (mod=00, reg=val_reg, rm=ptr_reg).
/// Note: callers must handle RSP (enc=4) and RBP (enc=5) edge cases separately.
fn modrm_mem(val_reg: PReg, ptr_reg: PReg) -> u8 {
    (reg_enc(val_reg) << 3) | reg_enc(ptr_reg)
}

/// Emit REX prefix for 64-bit memory-operand instructions.
/// REX.W always set; REX.R if val_reg is extended; REX.B if ptr_reg is extended.
fn rex_mem(ctx: &mut EncodeCtx, val_reg: PReg, ptr_reg: PReg) {
    let rex = 0x48
        | (if is_extended(val_reg) { 0x04 } else { 0 })
        | (if is_extended(ptr_reg) { 0x01 } else { 0 });
    ctx.emit(rex);
}

/// Emit an optional REX prefix for 32-bit memory-operand instructions (no REX.W).
/// Only emitted when at least one of val_reg or ptr_reg is an extended register (R8-R15).
fn rex_mem32_opt(ctx: &mut EncodeCtx, val_reg: PReg, ptr_reg: PReg) {
    let need_rex = is_extended(val_reg) || is_extended(ptr_reg);
    if need_rex {
        let rex = 0x40
            | (if is_extended(val_reg) { 0x04 } else { 0 })
            | (if is_extended(ptr_reg) { 0x01 } else { 0 });
        ctx.emit(rex);
    }
}

/// Extract the PReg at a given operand index.
fn preg_at(instr: &MInstr, idx: usize) -> Option<PReg> {
    match instr.operands.get(idx) {
        Some(MOperand::PReg(r)) => Some(*r),
        _ => None,
    }
}

/// Emit ModRM + optional SIB for [ptr_reg] addressing.
/// Handles RSP (SIB required) and RBP (must use mod=01+disp8=0) edge cases.
fn emit_mem_modrm(ctx: &mut EncodeCtx, val_reg: PReg, ptr_reg: PReg) {
    let ptr_enc = reg_enc(ptr_reg);
    if ptr_enc == 5 {
        // RBP/R13: mod=00 rm=5 means [RIP+disp32]; use mod=01 disp8=0 instead.
        ctx.emit(0x40 | (reg_enc(val_reg) << 3) | 5);
        ctx.emit(0x00); // disp8=0
    } else {
        ctx.emit(modrm_mem(val_reg, ptr_reg));
        if ptr_enc == 4 {
            // RSP/R12: rm=4 means SIB follows in mod=00; emit SIB=0x24 ([RSP]).
            ctx.emit(0x24);
        }
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
            ctx.emit(0x90);
        }
        INLINE_ASM => {
            if let Some(MOperand::Bytes(raw)) = instr.operands.first() {
                ctx.code.extend_from_slice(raw);
            }
        }

        // ── MOV reg, reg (REX.W 0x89 /r) ─────────────────────────────────
        MOV_RR => {
            if let (Some(dst), Some(src)) = (instr.dst, instr.operands.first().and_then(preg)) {
                // REX.W + MOV r/m64, r64: opcode 0x89 /r
                let dst_r = PReg(dst.0 as u8);
                maybe_rex(ctx, true, src, dst_r);
                ctx.emit(0x89);
                ctx.emit(modrm_rr(src, dst_r));
            } else {
                ctx.emit(0x90); // fallback NOP
            }
        }

        // ── MOV fixed_preg, src_preg (REX.W 0x89 /r) ────────────────────
        // operands[0] = PReg destination (ABI-fixed, not in `dst`),
        // operands[1] = PReg source (VReg resolved by regalloc).
        MOV_PR => {
            if let (Some(MOperand::PReg(dst)), Some(MOperand::PReg(src))) =
                (instr.operands.first(), instr.operands.get(1))
            {
                maybe_rex(ctx, true, *src, *dst);
                ctx.emit(0x89);
                ctx.emit(modrm_rr(*src, *dst));
            } else {
                ctx.emit(0x90); // fallback NOP (pre-allocation, should not reach encoder)
            }
        }

        // ── MOV reg, imm64 (REX.W 0xB8+rd) ───────────────────────────────
        MOV_RI => {
            if let (Some(dst), Some(val)) = (instr.dst, instr.operands.first().and_then(imm)) {
                // REX.W + MOV r64, imm64: opcode 0xB8 + rd
                let b_ext = is_extended(PReg(dst.0 as u8));
                ctx.emit(0x48 | (if b_ext { 0x01 } else { 0 }));
                ctx.emit(0xB8 | reg_enc(PReg(dst.0 as u8)));
                ctx.emit64(val);
            } else {
                ctx.emit(0x90);
            }
        }

        // ── ADD reg, reg (REX.W 0x01 /r) ─────────────────────────────────
        ADD_RR => {
            encode_rrr(ctx, instr, 0x01);
        }

        // ── ADD reg, imm32 (REX.W 0x81 /0 id) ────────────────────────────
        ADD_RI => {
            if let Some((dst, val)) = get_dst_preg_imm(instr) {
                maybe_rex(ctx, true, PReg(0), dst);
                ctx.emit(0x81);
                ctx.emit(0xC0 | reg_enc(dst)); // /0
                ctx.emit32(val as i32);
            } else {
                ctx.emit(0x90);
            }
        }

        // ── SUB reg, reg (REX.W 0x29 /r) ─────────────────────────────────
        SUB_RR => {
            encode_rrr(ctx, instr, 0x29);
        }

        // ── SUB reg, imm32 (REX.W 0x81 /5 id) ────────────────────────────
        SUB_RI => {
            if let Some((dst, val)) = get_dst_preg_imm(instr) {
                maybe_rex(ctx, true, PReg(0), dst);
                ctx.emit(0x81);
                ctx.emit(0xE8 | reg_enc(dst)); // /5
                ctx.emit32(val as i32);
            } else {
                ctx.emit(0x90);
            }
        }

        // ── IMUL dst, src (REX.W 0x0F 0xAF /r) ───────────────────────────
        IMUL_RR => {
            if let (Some(dst), Some(src)) = get_dst_src(instr) {
                maybe_rex(ctx, true, dst, src);
                ctx.emit(0x0F);
                ctx.emit(0xAF);
                ctx.emit(modrm_rr(dst, src));
            } else {
                ctx.emit(0x90);
            }
        }

        // ── IMUL dst, src, imm32 (REX.W 0x69 /r id) ─────────────────────
        IMUL_RRI => {
            if let Some((dst, src, imm32)) = get_dst_src_imm(instr) {
                maybe_rex(ctx, true, dst, src);
                ctx.emit(0x69);
                ctx.emit(modrm_rr(dst, src));
                ctx.emit32(imm32 as i32);
            } else {
                ctx.emit(0x90);
            }
        }

        // ── IDIV src (REX.W 0xF7 /7) ──────────────────────────────────────
        IDIV_R => {
            if let Some(src) = instr.operands.first().and_then(preg) {
                maybe_rex(ctx, true, PReg(0), src);
                ctx.emit(0xF7);
                ctx.emit(0xC0 | (7 << 3) | reg_enc(src)); // ModRM /7
            } else {
                ctx.emit(0x90);
            }
        }

        // ── DIV src (REX.W 0xF7 /6) — unsigned ───────────────────────────
        DIV_R => {
            if let Some(src) = instr.operands.first().and_then(preg) {
                maybe_rex(ctx, true, PReg(0), src);
                ctx.emit(0xF7);
                ctx.emit(0xC0 | (6 << 3) | reg_enc(src)); // ModRM /6
            } else {
                ctx.emit(0x90);
            }
        }

        // ── CQO (REX.W 0x99) ─────────────────────────────────────────────
        CQO => {
            ctx.emit(0x48); // REX.W
            ctx.emit(0x99);
        }

        // ── NEG reg (REX.W 0xF7 /3) ──────────────────────────────────────
        NEG_R => {
            if let Some(dst) = instr.dst {
                let r = PReg(dst.0 as u8);
                maybe_rex(ctx, true, PReg(0), r);
                ctx.emit(0xF7);
                ctx.emit(0xC0 | (3 << 3) | reg_enc(r));
            } else {
                ctx.emit(0x90);
            }
        }

        // ── AND reg, reg (REX.W 0x21 /r) ─────────────────────────────────
        AND_RR => {
            encode_rrr(ctx, instr, 0x21);
        }

        // ── OR reg, reg (REX.W 0x09 /r) ──────────────────────────────────
        OR_RR => {
            encode_rrr(ctx, instr, 0x09);
        }

        // ── XOR reg, reg (REX.W 0x31 /r) ─────────────────────────────────
        XOR_RR => {
            encode_rrr(ctx, instr, 0x31);
        }

        // ── NOT reg (REX.W 0xF7 /2) ──────────────────────────────────────
        NOT_R => {
            if let Some(dst) = instr.dst {
                let r = PReg(dst.0 as u8);
                maybe_rex(ctx, true, PReg(0), r);
                ctx.emit(0xF7);
                ctx.emit(0xC0 | (2 << 3) | reg_enc(r));
            } else {
                ctx.emit(0x90);
            }
        }

        // ── SHL reg, CL (REX.W 0xD3 /4) ─────────────────────────────────
        SHL_RR => {
            encode_shift_cl(ctx, instr, 4);
        }

        // ── SHR reg, CL (REX.W 0xD3 /5) ─────────────────────────────────
        SHR_RR => {
            encode_shift_cl(ctx, instr, 5);
        }

        // ── SAR reg, CL (REX.W 0xD3 /7) ─────────────────────────────────
        SAR_RR => {
            encode_shift_cl(ctx, instr, 7);
        }

        // ── CMP reg, reg (REX.W 0x39 /r) ─────────────────────────────────
        CMP_RR => {
            if let (Some(l), Some(r)) = get_two_pregs(instr) {
                maybe_rex(ctx, true, r, l);
                ctx.emit(0x39);
                ctx.emit(modrm_rr(r, l));
            } else {
                ctx.emit(0x90);
            }
        }

        // ── TEST reg, reg (REX.W 0x85 /r) ────────────────────────────────
        TEST_RR => {
            if let (Some(l), Some(r)) = get_two_pregs(instr) {
                maybe_rex(ctx, true, r, l);
                ctx.emit(0x85);
                ctx.emit(modrm_rr(r, l));
            } else {
                ctx.emit(0x90);
            }
        }

        // ── SETcc dst (REX? 0x0F 0x9x /0) ────────────────────────────────
        SETCC => {
            if let (Some(dst), Some(cc)) = (instr.dst, instr.operands.first().and_then(imm)) {
                let r = PReg(dst.0 as u8);
                // Without a REX prefix, 8-bit encodings 4-7 alias the high bytes
                // AH/CH/DH/BH rather than SPL/BPL/SIL/DIL.  A bare REX (0x40)
                // selects the low-byte form even when no register field extension
                // is needed; REX.B (0x41) selects R8-R15.
                if is_extended(r) {
                    ctx.emit(0x41); // REX.B for R8–R15
                } else if r.0 >= 4 {
                    ctx.emit(0x40); // bare REX for SIL/DIL (enc 6/7) and SPL/BPL (enc 4/5)
                }
                ctx.emit(0x0F);
                ctx.emit(setcc_opcode(cc));
                ctx.emit(0xC0 | reg_enc(r));
                // Zero-extend the 8-bit result to 64 bits via MOVZX r64, r8.
                maybe_rex(ctx, true, r, r);
                ctx.emit(0x0F);
                ctx.emit(0xB6);
                ctx.emit(modrm_rr(r, r));
            } else {
                ctx.emit(0x90);
            }
        }

        // ── JMP rel32 (0xE9 rel32) ────────────────────────────────────────
        JMP => {
            if let Some(MOperand::Block(target)) = instr.operands.first() {
                ctx.emit(0xE9);
                let patch_pos = ctx.pos();
                ctx.emit32(0); // placeholder
                ctx.branch_patches.push((patch_pos, *target));
            } else {
                ctx.emit(0x90);
            }
        }

        // ── JCC rel32 (0x0F 0x8x rel32) ──────────────────────────────────
        JCC => {
            if let (Some(MOperand::Imm(cc)), Some(MOperand::Block(target))) =
                (instr.operands.first(), instr.operands.get(1))
            {
                ctx.emit(0x0F);
                ctx.emit(jcc_opcode(*cc));
                let patch_pos = ctx.pos();
                ctx.emit32(0);
                ctx.branch_patches.push((patch_pos, *target));
            } else {
                ctx.emit(0x90);
            }
        }

        // ── CALL *reg (REX 0xFF /2) ───────────────────────────────────────
        CALL_R => {
            if let Some(src) = instr.operands.first().and_then(|op| match op {
                MOperand::PReg(r) => Some(*r),
                _ => None,
            }) {
                if is_extended(src) {
                    ctx.emit(0x41);
                }
                ctx.emit(0xFF);
                ctx.emit(0xC0 | (2 << 3) | reg_enc(src));
            } else {
                ctx.emit(0x90);
            }
        }

        // ── RET (0xC3) ────────────────────────────────────────────────────
        RET => {
            ctx.emit(0xC3);
        }

        // ── PUSH reg (REX? 0x50+rd) ───────────────────────────────────────
        PUSH_R => {
            if let Some(src) = instr.operands.first().and_then(preg) {
                if is_extended(src) {
                    ctx.emit(0x41);
                }
                ctx.emit(0x50 | reg_enc(src));
            } else {
                ctx.emit(0x90);
            }
        }

        // ── POP reg (REX? 0x58+rd) ────────────────────────────────────────
        POP_R => {
            if let Some(dst) = instr.dst {
                let r = PReg(dst.0 as u8);
                if is_extended(r) {
                    ctx.emit(0x41);
                }
                ctx.emit(0x58 | reg_enc(r));
            } else {
                ctx.emit(0x90);
            }
        }

        // ── MOVSX r64, r/m8  (REX.W 0x0F 0xBE /r) ───────────────────────
        MOVSX_8 => {
            if let (Some(dst), Some(src)) = get_dst_src(instr) {
                maybe_rex(ctx, true, dst, src);
                ctx.emit(0x0F);
                ctx.emit(0xBE);
                ctx.emit(modrm_rr(dst, src));
            } else {
                ctx.emit(0x90);
            }
        }

        // ── MOVSX r64, r/m16 (REX.W 0x0F 0xBF /r) ───────────────────────
        MOVSX_16 => {
            if let (Some(dst), Some(src)) = get_dst_src(instr) {
                maybe_rex(ctx, true, dst, src);
                ctx.emit(0x0F);
                ctx.emit(0xBF);
                ctx.emit(modrm_rr(dst, src));
            } else {
                ctx.emit(0x90);
            }
        }

        // ── MOVSX (sign-extend 32→64: REX.W 0x63 /r) ────────────────────
        MOVSX_32 => {
            if let (Some(dst), Some(src)) = get_dst_src(instr) {
                maybe_rex(ctx, true, dst, src);
                ctx.emit(0x63);
                ctx.emit(modrm_rr(dst, src));
            } else {
                ctx.emit(0x90);
            }
        }

        // ── MOV_LOAD_MR: mov dst, [rbp + disp] — spill reload ────────────
        // Spill slot `n` sits at [RBP - callee_save_bytes - alloca_frame_bytes - (n+1)*8].
        // Alloca slots occupy [callee_save_bytes, callee_save_bytes + alloca_frame_bytes),
        // and spill slots are placed below that.
        // Encoding: REX.W [REX.R if dst extended] 0x8B ModRM(10, dst, RBP=5) disp32
        MOV_LOAD_MR => {
            if let (Some(dst), Some(MOperand::Imm(slot))) = (instr.dst, instr.operands.first()) {
                let dst_r = PReg(dst.0 as u8);
                let disp = -((ctx.callee_save_bytes as i32)
                    + (ctx.alloca_frame_bytes as i32)
                    + (*slot as i32 + 1) * 8);
                // REX.W + (REX.R if dst extended)
                let rex = 0x48 | (if is_extended(dst_r) { 0x04 } else { 0 });
                ctx.emit(rex);
                ctx.emit(0x8B); // MOV r64, r/m64
                                // ModRM: mod=10 (mem+disp32), reg=dst_enc, rm=5 (RBP)
                ctx.emit(0x80 | (reg_enc(dst_r) << 3) | 5);
                ctx.emit32(disp);
            } else {
                ctx.emit(0x90);
            }
        }

        // ── MOV_STORE_RM: mov [rbp + disp], src — spill store ────────────
        // Encoding: REX.W [REX.R if src extended] 0x89 ModRM(10, src, RBP=5) disp32
        MOV_STORE_RM => {
            if let (Some(MOperand::Imm(slot)), Some(src)) = (
                instr.operands.first(),
                instr.operands.get(1).and_then(|op| match op {
                    MOperand::PReg(r) => Some(*r),
                    _ => None,
                }),
            ) {
                let disp = -((ctx.callee_save_bytes as i32)
                    + (ctx.alloca_frame_bytes as i32)
                    + (*slot as i32 + 1) * 8);
                let rex = 0x48 | (if is_extended(src) { 0x04 } else { 0 });
                ctx.emit(rex);
                ctx.emit(0x89); // MOV r/m64, r64
                ctx.emit(0x80 | (reg_enc(src) << 3) | 5);
                ctx.emit32(disp);
            } else {
                ctx.emit(0x90);
            }
        }

        // ── SIMD reg-reg operations (XMM) ─────────────────────────────────
        PADDD_RR => encode_simd_rr(ctx, instr, Some(0x66), &[0x0F, 0xFE]),
        PSUBD_RR => encode_simd_rr(ctx, instr, Some(0x66), &[0x0F, 0xFA]),
        PMULLD_RR => encode_simd_rr(ctx, instr, Some(0x66), &[0x0F, 0x38, 0x40]),
        ADDPS_RR => encode_simd_rr(ctx, instr, None, &[0x0F, 0x58]),
        MULPS_RR => encode_simd_rr(ctx, instr, None, &[0x0F, 0x59]),
        DIVPS_RR => encode_simd_rr(ctx, instr, None, &[0x0F, 0x5E]),
        ADDPD_RR => encode_simd_rr(ctx, instr, Some(0x66), &[0x0F, 0x58]),
        MULPD_RR => encode_simd_rr(ctx, instr, Some(0x66), &[0x0F, 0x59]),
        MOVAPS_RR => encode_simd_rr(ctx, instr, None, &[0x0F, 0x28]),

        // ── SIMD pseudo loads/stores using rbp+disp32 addressing ──────────
        MOVDQU_LOAD_MR => {
            if let (Some(dst), Some(MOperand::Imm(slot))) = (instr.dst, instr.operands.first()) {
                let dst_r = PReg(dst.0 as u8);
                let disp = -((ctx.callee_save_bytes as i32) + (*slot as i32 + 1) * 8);
                ctx.emit(0xF3);
                if is_extended(dst_r) {
                    ctx.emit(0x44); // REX.R
                }
                ctx.emit(0x0F);
                ctx.emit(0x6F);
                ctx.emit(0x80 | (reg_enc(dst_r) << 3) | 5);
                ctx.emit32(disp);
            } else {
                ctx.emit(0x90);
            }
        }
        MOVAPS_LOAD_MR => {
            if let (Some(dst), Some(MOperand::Imm(slot))) = (instr.dst, instr.operands.first()) {
                let dst_r = PReg(dst.0 as u8);
                let disp = -((ctx.callee_save_bytes as i32) + (*slot as i32 + 1) * 8);
                if is_extended(dst_r) {
                    ctx.emit(0x44); // REX.R
                }
                ctx.emit(0x0F);
                ctx.emit(0x28);
                ctx.emit(0x80 | (reg_enc(dst_r) << 3) | 5);
                ctx.emit32(disp);
            } else {
                ctx.emit(0x90);
            }
        }
        MOVDQU_STORE_RM => {
            if let (Some(MOperand::Imm(slot)), Some(src)) = (
                instr.operands.first(),
                instr.operands.get(1).and_then(|op| match op {
                    MOperand::PReg(r) => Some(*r),
                    _ => None,
                }),
            ) {
                let disp = -((ctx.callee_save_bytes as i32) + (*slot as i32 + 1) * 8);
                ctx.emit(0xF3);
                if is_extended(src) {
                    ctx.emit(0x44); // REX.R
                }
                ctx.emit(0x0F);
                ctx.emit(0x7F);
                ctx.emit(0x80 | (reg_enc(src) << 3) | 5);
                ctx.emit32(disp);
            } else {
                ctx.emit(0x90);
            }
        }

        // ── atomic memory operations ──────────────────────────────────────
        MFENCE => {
            ctx.emit(0x0F);
            ctx.emit(0xAE);
            ctx.emit(0xF0);
        }

        // LOCK CMPXCHG [ptr], new_val  (F0 REX.W 0F B1 /r)
        // operands[0]=ptr_preg, operands[1]=RAX (comparand), operands[2]=new_val_preg
        LOCK_CMPXCHG_MR => {
            if let (Some(ptr), Some(new_v)) = (preg_at(instr, 0), preg_at(instr, 2)) {
                ctx.emit(0xF0); // LOCK prefix
                rex_mem(ctx, new_v, ptr);
                ctx.emit(0x0F);
                ctx.emit(0xB1);
                emit_mem_modrm(ctx, new_v, ptr);
            } else {
                ctx.emit(0x90);
            }
        }

        // LOCK XADD [ptr], val  (F0 REX.W 0F C1 /r)
        // operands[0]=ptr_preg, operands[1]=val_preg
        LOCK_XADD_MR => {
            if let (Some(ptr), Some(val)) = (preg_at(instr, 0), preg_at(instr, 1)) {
                ctx.emit(0xF0);
                rex_mem(ctx, val, ptr);
                ctx.emit(0x0F);
                ctx.emit(0xC1);
                emit_mem_modrm(ctx, val, ptr);
            } else {
                ctx.emit(0x90);
            }
        }

        // LOCK XADD [ptr], val  (F0 [REX] 0F C1 /r) — 32-bit, no REX.W
        // operands[0]=ptr_preg, operands[1]=val_preg
        LOCK_XADD32_MR => {
            if let (Some(ptr), Some(val)) = (preg_at(instr, 0), preg_at(instr, 1)) {
                ctx.emit(0xF0);
                rex_mem32_opt(ctx, val, ptr);
                ctx.emit(0x0F);
                ctx.emit(0xC1);
                emit_mem_modrm(ctx, val, ptr);
            } else {
                ctx.emit(0x90);
            }
        }

        // XCHG [ptr], val  (REX.W 87 /r) — implicitly LOCK
        // operands[0]=ptr_preg, operands[1]=val_preg
        XCHG_MR => {
            if let (Some(ptr), Some(val)) = (preg_at(instr, 0), preg_at(instr, 1)) {
                rex_mem(ctx, val, ptr);
                ctx.emit(0x87);
                emit_mem_modrm(ctx, val, ptr);
            } else {
                ctx.emit(0x90);
            }
        }

        // LOCK AND [ptr], val  (F0 REX.W 21 /r)
        LOCK_AND_MR => {
            if let (Some(ptr), Some(val)) = (preg_at(instr, 0), preg_at(instr, 1)) {
                ctx.emit(0xF0);
                rex_mem(ctx, val, ptr);
                ctx.emit(0x21);
                emit_mem_modrm(ctx, val, ptr);
            } else {
                ctx.emit(0x90);
            }
        }

        // LOCK OR [ptr], val  (F0 REX.W 09 /r)
        LOCK_OR_MR => {
            if let (Some(ptr), Some(val)) = (preg_at(instr, 0), preg_at(instr, 1)) {
                ctx.emit(0xF0);
                rex_mem(ctx, val, ptr);
                ctx.emit(0x09);
                emit_mem_modrm(ctx, val, ptr);
            } else {
                ctx.emit(0x90);
            }
        }

        // LOCK XOR [ptr], val  (F0 REX.W 31 /r)
        LOCK_XOR_MR => {
            if let (Some(ptr), Some(val)) = (preg_at(instr, 0), preg_at(instr, 1)) {
                ctx.emit(0xF0);
                rex_mem(ctx, val, ptr);
                ctx.emit(0x31);
                emit_mem_modrm(ctx, val, ptr);
            } else {
                ctx.emit(0x90);
            }
        }

        // ── LEA (placeholder: encode as MOV_RI 0) ────────────────────────
        LEA_RI => {
            // Simplified: emit xor reg, reg (zero the register).
            if let Some(dst) = instr.dst {
                let r = PReg(dst.0 as u8);
                maybe_rex(ctx, true, r, r);
                ctx.emit(0x31);
                ctx.emit(modrm_rr(r, r));
            } else {
                ctx.emit(0x90);
            }
        }

        // ── LEA_FRAME_MR: lea dst, [rbp - (callee_save_bytes + (slot_idx+1)*8)] ─
        // Alloca slot `slot_idx` sits directly below the callee-saved register saves.
        // Layout: RBP - callee_save_bytes - (slot_idx+1)*8
        // Encoding: REX.W 8D /r ModRM(mod=10, reg=dst, rm=5=RBP) disp32
        LEA_FRAME_MR => {
            if let (Some(dst), Some(MOperand::Imm(slot_idx))) = (instr.dst, instr.operands.first()) {
                let dst_r = PReg(dst.0 as u8);
                let disp = -((ctx.callee_save_bytes as i32) + (*slot_idx as i32 + 1) * 8);
                let rex = 0x48 | (if is_extended(dst_r) { 0x04 } else { 0 });
                ctx.emit(rex);
                ctx.emit(0x8D); // LEA r64, m
                // ModRM: mod=10 (disp32), reg=dst_enc, rm=5 (RBP)
                ctx.emit(0x80 | (reg_enc(dst_r) << 3) | 5);
                ctx.emit32(disp);
            } else {
                ctx.emit(0x90);
            }
        }

        // ── MOV_LOAD_REG_MR: mov dst, [ptr_reg] ──────────────────────────
        // 64-bit load through a register-held pointer (no displacement).
        // Encoding: REX.W 8B /r with mod=00 rm=ptr_reg (RBP/RSP edge cases handled).
        MOV_LOAD_REG_MR => {
            if let (Some(dst), Some(ptr)) = (instr.dst, preg_at(instr, 0)) {
                let dst_r = PReg(dst.0 as u8);
                rex_mem(ctx, dst_r, ptr);
                ctx.emit(0x8B); // MOV r64, r/m64
                emit_mem_modrm(ctx, dst_r, ptr);
            } else {
                ctx.emit(0x90);
            }
        }

        // ── MOV_STORE_REG_RM: mov [ptr_reg], src ─────────────────────────
        // 64-bit store through a register-held pointer (no displacement).
        // Encoding: REX.W 89 /r with mod=00 rm=ptr_reg.
        MOV_STORE_REG_RM => {
            if let (Some(ptr), Some(src)) = (preg_at(instr, 0), preg_at(instr, 1)) {
                rex_mem(ctx, src, ptr);
                ctx.emit(0x89); // MOV r/m64, r64
                emit_mem_modrm(ctx, src, ptr);
            } else {
                ctx.emit(0x90);
            }
        }

        // ── SSE2 scalar double: 66 0F <op> /r ─────────────────────────────
        ADDSD_RR  => emit_sse2_rr(ctx, instr, 0x66, 0x58),
        SUBSD_RR  => emit_sse2_rr(ctx, instr, 0x66, 0x5C),
        MULSD_RR  => emit_sse2_rr(ctx, instr, 0x66, 0x59),
        DIVSD_RR  => emit_sse2_rr(ctx, instr, 0x66, 0x5E),
        SQRTSD_R  => emit_sse2_rr(ctx, instr, 0x66, 0x51),
        UCOMISD_RR => emit_sse2_cmp(ctx, instr, 0x66, 0x2E),
        MOVAPD_RR | MOVAPD_RR_F32 => emit_sse2_rr(ctx, instr, 0x66, 0x28),

        // ── SSE2 scalar single: F3 0F <op> /r ─────────────────────────────
        ADDSS_RR  => emit_sse2_rr(ctx, instr, 0xF3, 0x58),
        SUBSS_RR  => emit_sse2_rr(ctx, instr, 0xF3, 0x5C),
        MULSS_RR  => emit_sse2_rr(ctx, instr, 0xF3, 0x59),
        DIVSS_RR  => emit_sse2_rr(ctx, instr, 0xF3, 0x5E),
        SQRTSS_R  => emit_sse2_rr(ctx, instr, 0xF3, 0x51),
        UCOMISS_RR => emit_sse2_cmp(ctx, instr, 0x00, 0x2E), // no mandatory prefix for SSE UCOMISS
        MOVSS_LOAD_MR  => emit_sse2_spill_load(ctx, instr, 0xF3, 0x10),
        MOVSS_STORE_RM => emit_sse2_spill_store(ctx, instr, 0xF3, 0x11),

        // ── FP spill load/store: F2 0F 10/11 [rbp+disp32] ─────────────────
        MOVSD_LOAD_MR  => emit_sse2_spill_load(ctx, instr, 0xF2, 0x10),
        MOVSD_STORE_RM => emit_sse2_spill_store(ctx, instr, 0xF2, 0x11),

        // ── FP ↔ int conversions ──────────────────────────────────────────
        CVTTSD2SI_RR => emit_cvt_xmm_to_gpr(ctx, instr, 0xF2, 0x2C),
        CVTSI2SD_RR  => emit_cvt_gpr_to_xmm(ctx, instr, 0xF2, 0x2A),
        CVTTSS2SI_RR => emit_cvt_xmm_to_gpr(ctx, instr, 0xF3, 0x2C),
        CVTSI2SS_RR  => emit_cvt_gpr_to_xmm(ctx, instr, 0xF3, 0x2A),
        CVTSD2SS_RR  => emit_sse2_rr(ctx, instr, 0xF2, 0x5A),
        CVTSS2SD_RR  => emit_sse2_rr(ctx, instr, 0xF3, 0x5A),

        // ── unsupported: emit NOP ─────────────────────────────────────────
        _ => {
            ctx.emit(0x90);
        }
    }
}

// ── SSE2 encoding helpers ────────────────────────────────────────────────

/// Emit a REX byte for XMM ← XMM operations when extended registers are used.
/// REX.R = dst XMM8-15; REX.B = src XMM8-15.  No REX.W for SSE2 scalar ops.
fn xmm_rex(ctx: &mut EncodeCtx, dst_enc: u8, src_enc: u8) {
    let rex = 0x40u8
        | (if dst_enc >= 8 { 0x04 } else { 0 })
        | (if src_enc >= 8 { 0x01 } else { 0 });
    if rex != 0x40 {
        ctx.emit(rex);
    }
}

/// ModRM byte for XMM-XMM: mod=11, reg=dst_enc&7, rm=src_enc&7.
fn xmm_modrm(dst_enc: u8, src_enc: u8) -> u8 {
    0xC0 | ((dst_enc & 7) << 3) | (src_enc & 7)
}

/// Emit an SSE2 reg-reg instruction: [mandatory_prefix] [REX] 0F <op> /r.
/// `mandatory_prefix` = 0x66 (SD), 0xF3 (SS), 0xF2 (SD with F2 prefix).
/// Handles both XMM←XMM and the dst-is-VReg path after regalloc (both operands
/// decoded as PReg post-allocation).
fn emit_sse2_rr(ctx: &mut EncodeCtx, instr: &MInstr, prefix: u8, op: u8) {
    let (dst, src) = get_xmm_dst_src(instr);
    if let (Some(d), Some(s)) = (dst, src) {
        if prefix != 0x00 {
            ctx.emit(prefix);
        }
        xmm_rex(ctx, xmm_enc(d), xmm_enc(s));
        ctx.emit(0x0F);
        ctx.emit(op);
        ctx.emit(xmm_modrm(xmm_enc(d), xmm_enc(s)));
    } else {
        ctx.emit(0x90);
    }
}

/// Emit UCOMISD/UCOMISS (no destination; two source XMM registers).
/// Layout: [prefix] [REX] 0F 2E /r  where reg=lhs, rm=rhs.
fn emit_sse2_cmp(ctx: &mut EncodeCtx, instr: &MInstr, prefix: u8, op: u8) {
    let (lhs, rhs) = get_two_xmm(instr);
    if let (Some(l), Some(r)) = (lhs, rhs) {
        if prefix != 0x00 {
            ctx.emit(prefix);
        }
        xmm_rex(ctx, xmm_enc(l), xmm_enc(r));
        ctx.emit(0x0F);
        ctx.emit(op);
        ctx.emit(xmm_modrm(xmm_enc(l), xmm_enc(r)));
    } else {
        ctx.emit(0x90);
    }
}

/// Emit a spill-reload MOVSD/MOVSS: [prefix] [REX] 0F 10 dst, [rbp+disp32].
fn emit_sse2_spill_load(ctx: &mut EncodeCtx, instr: &MInstr, prefix: u8, op: u8) {
    if let (Some(dst), Some(MOperand::Imm(slot))) = (instr.dst, instr.operands.first()) {
        let dst_r = PReg(dst.0 as u8);
        let dst_enc = xmm_enc(dst_r);
        let disp = -((ctx.callee_save_bytes as i32) + (*slot as i32 + 1) * 8);
        ctx.emit(prefix);
        if dst_enc >= 8 {
            ctx.emit(0x44); // REX.R
        }
        ctx.emit(0x0F);
        ctx.emit(op);
        // ModRM: mod=10 (disp32), reg=dst_enc&7, rm=5 (RBP)
        ctx.emit(0x80 | ((dst_enc & 7) << 3) | 5);
        ctx.emit32(disp);
    } else {
        ctx.emit(0x90);
    }
}

/// Emit a spill-store MOVSD/MOVSS: [prefix] [REX] 0F 11 src, [rbp+disp32].
fn emit_sse2_spill_store(ctx: &mut EncodeCtx, instr: &MInstr, prefix: u8, op: u8) {
    if let (Some(MOperand::Imm(slot)), Some(src)) = (
        instr.operands.first(),
        instr.operands.get(1).and_then(|o| match o {
            MOperand::PReg(r) => Some(*r),
            _ => None,
        }),
    ) {
        let src_enc = xmm_enc(src);
        let disp = -((ctx.callee_save_bytes as i32) + (*slot as i32 + 1) * 8);
        ctx.emit(prefix);
        if src_enc >= 8 {
            ctx.emit(0x44); // REX.R (src goes in reg field of ModRM)
        }
        ctx.emit(0x0F);
        ctx.emit(op);
        // ModRM: mod=10 (disp32), reg=src_enc&7, rm=5 (RBP)
        ctx.emit(0x80 | ((src_enc & 7) << 3) | 5);
        ctx.emit32(disp);
    } else {
        ctx.emit(0x90);
    }
}

/// Emit CVTTSD2SI / CVTTSS2SI: [prefix] REX.W 0F 2C /r  (XMM → GPR).
fn emit_cvt_xmm_to_gpr(ctx: &mut EncodeCtx, instr: &MInstr, prefix: u8, op: u8) {
    if let (Some(dst_vr), Some(src)) = (instr.dst, get_xmm_src(instr)) {
        let dst = PReg(dst_vr.0 as u8);
        let src_enc = xmm_enc(src);
        ctx.emit(prefix);
        // REX.W always for 64-bit GPR dst; REX.R if dst GPR is R8-R15; REX.B if src XMM8-15.
        let rex = 0x48
            | (if is_extended(dst) { 0x04 } else { 0 })
            | (if src_enc >= 8 { 0x01 } else { 0 });
        ctx.emit(rex);
        ctx.emit(0x0F);
        ctx.emit(op);
        ctx.emit(0xC0 | (reg_enc(dst) << 3) | (src_enc & 7));
    } else {
        ctx.emit(0x90);
    }
}

/// Emit CVTSI2SD / CVTSI2SS: [prefix] REX.W 0F 2A /r  (GPR → XMM).
fn emit_cvt_gpr_to_xmm(ctx: &mut EncodeCtx, instr: &MInstr, prefix: u8, op: u8) {
    if let (Some(dst_vr), Some(src)) = (instr.dst, get_gpr_src(instr)) {
        let dst = PReg(dst_vr.0 as u8);
        let dst_enc = xmm_enc(dst);
        ctx.emit(prefix);
        // REX.W for 64-bit GPR src; REX.R if dst XMM8-15; REX.B if src GPR is R8-R15.
        let rex = 0x48
            | (if dst_enc >= 8 { 0x04 } else { 0 })
            | (if is_extended(src) { 0x01 } else { 0 });
        ctx.emit(rex);
        ctx.emit(0x0F);
        ctx.emit(op);
        ctx.emit(0xC0 | ((dst_enc & 7) << 3) | reg_enc(src));
    } else {
        ctx.emit(0x90);
    }
}

/// Extract (dst_xmm, src_xmm) from an SSE2 binary instruction post-allocation.
/// dst lives in instr.dst (VReg whose id = PReg number after apply_allocation).
/// src is in the first PReg operand.
fn get_xmm_dst_src(instr: &MInstr) -> (Option<PReg>, Option<PReg>) {
    let dst = instr.dst.map(|v| PReg(v.0 as u8)).filter(|r| is_fp_reg(*r));
    let src = instr.operands.iter().find_map(|op| match op {
        MOperand::PReg(r) if is_fp_reg(*r) => Some(*r),
        _ => None,
    });
    (dst, src)
}

/// Extract two XMM PReg operands (for UCOMISD/UCOMISS which have no dst).
fn get_two_xmm(instr: &MInstr) -> (Option<PReg>, Option<PReg>) {
    let mut it = instr.operands.iter().filter_map(|op| match op {
        MOperand::PReg(r) if is_fp_reg(*r) => Some(*r),
        _ => None,
    });
    (it.next(), it.next())
}

/// Extract the source XMM PReg from a unary XMM instruction.
fn get_xmm_src(instr: &MInstr) -> Option<PReg> {
    instr.operands.iter().find_map(|op| match op {
        MOperand::PReg(r) if is_fp_reg(*r) => Some(*r),
        _ => None,
    })
}

/// Extract the source GPR PReg from a CVT instruction (XMM ← GPR).
fn get_gpr_src(instr: &MInstr) -> Option<PReg> {
    instr.operands.iter().find_map(|op| match op {
        MOperand::PReg(r) if !is_fp_reg(*r) => Some(*r),
        _ => None,
    })
}

// ── encoding helpers ─────────────────────────────────────────────────────

/// Encode a 3-reg binary op: `opcode r/m64, r64` (mod=11).
/// Expects `instr.dst` = destination (also first source), second src in operands[1].
fn encode_rrr(ctx: &mut EncodeCtx, instr: &MInstr, opcode: u8) {
    if let (Some(dst), Some(src)) = get_dst_src(instr) {
        maybe_rex(ctx, true, src, dst);
        ctx.emit(opcode);
        ctx.emit(modrm_rr(src, dst));
    } else {
        ctx.emit(0x90);
    }
}

/// Encode a shift by CL: `opcode r/m64, CL` (REX.W 0xD3 /ext).
fn encode_shift_cl(ctx: &mut EncodeCtx, instr: &MInstr, ext: u8) {
    if let Some(dst) = instr.dst {
        let r = PReg(dst.0 as u8);
        maybe_rex(ctx, true, PReg(0), r);
        ctx.emit(0xD3);
        ctx.emit(0xC0 | (ext << 3) | reg_enc(r));
    } else {
        ctx.emit(0x90);
    }
}

fn encode_simd_rr(ctx: &mut EncodeCtx, instr: &MInstr, legacy_prefix: Option<u8>, opcode: &[u8]) {
    if let (Some(dst), Some(src)) = get_dst_src(instr) {
        if let Some(pfx) = legacy_prefix {
            ctx.emit(pfx);
        }
        if is_extended(dst) || is_extended(src) {
            ctx.emit(
                0x40 | (if is_extended(dst) { 0x04 } else { 0 })
                    | (if is_extended(src) { 0x01 } else { 0 }),
            );
        }
        for b in opcode {
            ctx.emit(*b);
        }
        ctx.emit(modrm_rr(dst, src));
    } else {
        ctx.emit(0x90);
    }
}

/// Extract (dst_preg, src_preg) from an instruction where dst is also first
/// operand and second operand is another PReg.
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

fn get_dst_preg_imm(instr: &MInstr) -> Option<(PReg, i64)> {
    if let (Some(dst), Some(MOperand::Imm(v))) = (instr.dst, instr.operands.first()) {
        return Some((PReg(dst.0 as u8), *v));
    }
    if let (Some(MOperand::PReg(dst)), Some(MOperand::Imm(v))) =
        (instr.operands.first(), instr.operands.get(1))
    {
        return Some((*dst, *v));
    }
    None
}

fn get_dst_src_imm(instr: &MInstr) -> Option<(PReg, PReg, i64)> {
    let dst = instr.dst.map(|v| PReg(v.0 as u8))?;
    let src = instr.operands.iter().find_map(|op| {
        if let MOperand::PReg(r) = op {
            Some(*r)
        } else {
            None
        }
    })?;
    let imm = instr.operands.iter().find_map(|op| {
        if let MOperand::Imm(v) = op {
            Some(*v)
        } else {
            None
        }
    })?;
    Some((dst, src, imm))
}

/// Map a CC_* constant to the SETcc opcode byte (second byte of 0x0F 0x9x).
fn setcc_opcode(cc: i64) -> u8 {
    match cc {
        CC_EQ => 0x94,  // SETE
        CC_NE => 0x95,  // SETNE
        CC_LT => 0x9C,  // SETL
        CC_LE => 0x9E,  // SETLE
        CC_GT => 0x9F,  // SETG
        CC_GE => 0x9D,  // SETGE
        CC_ULT => 0x92, // SETB
        CC_ULE => 0x96, // SETBE
        CC_UGT => 0x97, // SETA
        CC_UGE => 0x93, // SETAE
        _ => 0x94,
    }
}

/// Map a CC_* constant to the Jcc opcode byte (second byte of 0x0F 0x8x).
fn jcc_opcode(cc: i64) -> u8 {
    match cc {
        CC_EQ => 0x84,  // JE
        CC_NE => 0x85,  // JNE
        CC_LT => 0x8C,  // JL
        CC_LE => 0x8E,  // JLE
        CC_GT => 0x8F,  // JG
        CC_GE => 0x8D,  // JGE
        CC_ULT => 0x82, // JB
        CC_ULE => 0x86, // JBE
        CC_UGT => 0x87, // JA
        CC_UGE => 0x83, // JAE
        _ => 0x84,
    }
}

// ── tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::regs::{RAX, RDI, RSI, RSP};
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
    fn nop_encodes_to_0x90() {
        let mf = single_block_mf("nop_fn", vec![MInstr::new(NOP)]);
        let mut e = X86Emitter::new(ObjectFormat::Elf);
        let sec = e.emit_function(&mf);
        assert_eq!(sec.data, vec![0x90]);
    }

    #[test]
    fn ret_encodes_to_0xc3() {
        let mf = single_block_mf("ret_fn", vec![MInstr::new(RET)]);
        let mut e = X86Emitter::new(ObjectFormat::Elf);
        let sec = e.emit_function(&mf);
        assert_eq!(sec.data, vec![0xC3]);
    }

    #[test]
    fn cqo_encodes_correctly() {
        let mf = single_block_mf("cqo_fn", vec![MInstr::new(CQO)]);
        let mut e = X86Emitter::new(ObjectFormat::Elf);
        let sec = e.emit_function(&mf);
        assert_eq!(sec.data, vec![0x48, 0x99]);
    }

    #[test]
    fn mov_rr_rax_rdi() {
        // mov rax, rdi  → REX.W (0x48) + 0x89 + ModRM
        let v0 = VReg(0);
        let mi = MInstr::new(MOV_RR).with_dst(v0).with_preg(RDI);
        // After regalloc, dst would be a PReg; here we test with PReg directly
        // by building a simpler instruction that the encoder recognises.
        // mov rax, rsi:  REX.W=0x48, opcode=0x89, modrm=11_110_000=0xF0
        let mi2 = MInstr {
            opcode: MOV_RR,
            dst: Some(VReg(RAX.0 as u32)),
            operands: vec![MOperand::PReg(RSI)],
            phys_uses: vec![],
            clobbers: vec![],
            debug_loc: None,
        };
        let mf = single_block_mf("mov_fn", vec![mi2]);
        let mut e = X86Emitter::new(ObjectFormat::Elf);
        let sec = e.emit_function(&mf);
        // REX.W=0x48, MOV r/m64,r64=0x89, ModRM(11 110 000)=0xF0
        assert_eq!(&sec.data[0..3], &[0x48, 0x89, 0xF0]);
        let _ = mi;
    }

    #[test]
    fn setcc_rsi_emits_bare_rex() {
        // Issue #35: SETCC into RSI (encoding 6) must emit a bare REX (0x40) before
        // the 0x0F opcode so that encoding 6 selects SIL (not DH which lacks REX).
        // Instruction: SETCC with dst=RSI, condition=CC_EQ (0x94).
        // Expected prefix: 0x40 (bare REX), then 0x0F 0x94 0xC6 (SETE sil).
        use crate::regs::RSI;
        let mi = MInstr {
            opcode: SETCC,
            dst: Some(VReg(RSI.0 as u32)),
            operands: vec![MOperand::Imm(CC_EQ)],
            phys_uses: vec![],
            clobbers: vec![],
            debug_loc: None,
        };
        let mf = single_block_mf("setcc_fn", vec![mi]);
        let mut e = X86Emitter::new(ObjectFormat::Elf);
        let sec = e.emit_function(&mf);
        // First 4 bytes: bare REX(0x40), 0x0F, SETE(0x94), ModRM(0xC6 = 11_000_110 for RSI)
        assert_eq!(
            sec.data[0], 0x40,
            "bare REX must be emitted for SETCC into RSI"
        );
        assert_eq!(sec.data[1], 0x0F, "escape prefix");
        assert_eq!(sec.data[2], 0x94, "SETE opcode byte");
        assert_eq!(sec.data[3], 0xC6, "ModRM(11 000 110) for RSI");
    }

    #[test]
    fn setcc_rax_no_extra_rex() {
        // RAX (encoding 0) does not need a REX prefix for 8-bit access — AL is
        // directly addressable. Verify no spurious REX appears before 0x0F.
        let mi = MInstr {
            opcode: SETCC,
            dst: Some(VReg(RAX.0 as u32)),
            operands: vec![MOperand::Imm(CC_EQ)],
            phys_uses: vec![],
            clobbers: vec![],
            debug_loc: None,
        };
        let mf = single_block_mf("setcc_rax_fn", vec![mi]);
        let mut e = X86Emitter::new(ObjectFormat::Elf);
        let sec = e.emit_function(&mf);
        // First byte must be 0x0F (no REX prefix for RAX).
        assert_eq!(
            sec.data[0], 0x0F,
            "no REX prefix should be emitted for SETCC into RAX"
        );
    }

    #[test]
    fn div_r_encodes_correctly() {
        // div rcx → REX.W(0x48) + 0xF7 + ModRM(/6, rcx=1) = 0xF1
        use crate::regs::RCX;
        let mi = MInstr {
            opcode: DIV_R,
            dst: None,
            operands: vec![MOperand::PReg(RCX)],
            phys_uses: vec![],
            clobbers: vec![],
            debug_loc: None,
        };
        let mf = single_block_mf("div_fn", vec![mi]);
        let mut e = X86Emitter::new(ObjectFormat::Elf);
        let sec = e.emit_function(&mf);
        // REX.W=0x48, F7, ModRM(11 110 001) = 0xF1 (digit /6 = 110b, rcx=001b)
        assert_eq!(
            &sec.data[0..3],
            &[0x48, 0xF7, 0xF1],
            "div rcx should encode as REX.W + 0xF7 + ModRM(/6)"
        );
    }

    #[test]
    fn idiv_r_encodes_correctly() {
        // idiv rcx → REX.W(0x48) + 0xF7 + ModRM(/7, rcx=1) = 0xF9
        use crate::regs::RCX;
        let mi = MInstr {
            opcode: IDIV_R,
            dst: None,
            operands: vec![MOperand::PReg(RCX)],
            phys_uses: vec![],
            clobbers: vec![],
            debug_loc: None,
        };
        let mf = single_block_mf("idiv_fn", vec![mi]);
        let mut e = X86Emitter::new(ObjectFormat::Elf);
        let sec = e.emit_function(&mf);
        // REX.W=0x48, F7, ModRM(11 111 001) = 0xF9 (digit /7 = 111b, rcx=001b)
        assert_eq!(
            &sec.data[0..3],
            &[0x48, 0xF7, 0xF9],
            "idiv rcx should encode as REX.W + 0xF7 + ModRM(/7)"
        );
    }

    #[test]
    fn mov_pr_rax_rsi_encodes_correctly() {
        // MOV_PR: mov rax, rsi
        // operands[0] = PReg(RAX=0), operands[1] = PReg(RSI=6)
        // Expected: REX.W(0x48) + MOV r/m64,r64(0x89) + ModRM(11_110_000=0xF0)
        use llvm_codegen::isel::MOperand;
        let mi = MInstr {
            opcode: MOV_PR,
            dst: None,
            operands: vec![MOperand::PReg(RAX), MOperand::PReg(RSI)],
            phys_uses: vec![],
            clobbers: vec![],
            debug_loc: None,
        };
        let mf = single_block_mf("mov_pr_fn", vec![mi]);
        let mut e = X86Emitter::new(ObjectFormat::Elf);
        let sec = e.emit_function(&mf);
        assert_eq!(
            &sec.data[0..3],
            &[0x48, 0x89, 0xF0],
            "mov rax, rsi should be REX.W + 0x89 + ModRM(11 110 000)"
        );
    }

    #[test]
    fn mov_pr_emits_non_nop_for_extended_reg() {
        // MOV_PR: mov r8, rdi (R8 is an extended register, needs REX.B)
        // Expected: REX.WB(0x49) + 0x89 + ModRM(11_111_000=0xF8)
        use crate::regs::{R8, RDI};
        use llvm_codegen::isel::MOperand;
        let mi = MInstr {
            opcode: MOV_PR,
            dst: None,
            operands: vec![MOperand::PReg(R8), MOperand::PReg(RDI)],
            phys_uses: vec![],
            clobbers: vec![],
            debug_loc: None,
        };
        let mf = single_block_mf("mov_pr_ext_fn", vec![mi]);
        let mut e = X86Emitter::new(ObjectFormat::Elf);
        let sec = e.emit_function(&mf);
        // REX.W + REX.B = 0x49, opcode = 0x89, ModRM(11 111 000) = 0xF8
        assert_eq!(
            &sec.data[0..3],
            &[0x49, 0x89, 0xF8],
            "mov r8, rdi should use REX.WB"
        );
    }

    #[test]
    fn jmp_patches_offset() {
        // Two blocks: block 0 jumps to block 1 which has a RET.
        let mut mf = MachineFunction::new("jmp_fn".into());
        let b0 = mf.add_block("entry");
        let b1 = mf.add_block("exit");
        mf.push(b0, MInstr::new(JMP).with_block(b1));
        mf.push(b1, MInstr::new(RET));

        let mut e = X86Emitter::new(ObjectFormat::Elf);
        let sec = e.emit_function(&mf);

        // JMP is 5 bytes (0xE9 + rel32), RET is 1 byte (0xC3).
        // JMP should target 0 bytes after itself → rel32 = 0.
        assert_eq!(sec.data.len(), 6);
        assert_eq!(sec.data[0], 0xE9, "JMP opcode");
        let rel = i32::from_le_bytes([sec.data[1], sec.data[2], sec.data[3], sec.data[4]]);
        assert_eq!(
            rel, 0,
            "JMP should jump 0 bytes forward (to adjacent block)"
        );
        assert_eq!(sec.data[5], 0xC3, "RET opcode");
    }

    #[test]
    fn elf_object_contains_text_section() {
        let mf = single_block_mf("fn1", vec![MInstr::new(RET)]);
        let mut e = X86Emitter::new(ObjectFormat::Elf);
        let obj = emit_object(&mf, &mut e);
        let bytes = obj.to_bytes();
        // Check ELF magic.
        assert_eq!(&bytes[0..4], b"\x7fELF");
    }

    #[test]
    fn macho_object_contains_ret() {
        let mf = single_block_mf("fn2", vec![MInstr::new(RET)]);
        let mut e = X86Emitter::new(ObjectFormat::MachO);
        let sec = e.emit_function(&mf);
        assert!(sec.data.contains(&0xC3), "RET byte must be in code");
    }

    #[test]
    fn mov_load_mr_encodes_correctly() {
        // MOV_LOAD_MR: mov rax, [rbp + disp]  — slot 0, no callee-saved pushes.
        // disp = -(0 + (0+1)*8) = -8
        // REX.W(0x48) + 0x8B + ModRM(10, RAX=0, RBP=5 → 0x80|0<<3|5=0x85) + disp32(-8)
        use crate::instructions::MOV_LOAD_MR;
        let mi = MInstr {
            opcode: MOV_LOAD_MR,
            dst: Some(VReg(RAX.0 as u32)),
            operands: vec![MOperand::Imm(0)], // slot 0
            phys_uses: vec![],
            clobbers: vec![],
            debug_loc: None,
        };
        let mf = single_block_mf("load_fn", vec![mi]);
        let mut e = X86Emitter::new(ObjectFormat::Elf);
        let sec = e.emit_function(&mf);
        // Expect: REX.W=0x48, 0x8B, ModRM=0x45 (mod=01? or mod=10?), disp32
        // For disp=-8 (fits in i32 but not i8 for this encoding form):
        // Actually with ModRM mod=10 (mem+disp32): 0x80 | (0<<3) | 5 = 0x85
        assert_eq!(sec.data[0], 0x48, "REX.W prefix");
        assert_eq!(sec.data[1], 0x8B, "MOV r64, r/m64 opcode");
        assert_eq!(sec.data[2], 0x85, "ModRM(mod=10, reg=RAX=0, rm=RBP=5)");
        // disp32 = -8 = 0xFFFFFFF8 in LE: [0xF8, 0xFF, 0xFF, 0xFF]
        assert_eq!(&sec.data[3..7], &[0xF8, 0xFF, 0xFF, 0xFF], "disp32 = -8");
    }

    #[test]
    fn mov_store_rm_encodes_correctly() {
        // MOV_STORE_RM: mov [rbp + disp], rax — slot 0, no callee-saved pushes.
        // disp = -8
        // REX.W(0x48) + 0x89 + ModRM(10, RAX=0, RBP=5 → 0x85) + disp32(-8)
        use crate::instructions::MOV_STORE_RM;
        let mi = MInstr {
            opcode: MOV_STORE_RM,
            dst: None,
            operands: vec![MOperand::Imm(0), MOperand::PReg(RAX)], // slot 0, src=RAX
            phys_uses: vec![],
            clobbers: vec![],
            debug_loc: None,
        };
        let mf = single_block_mf("store_fn", vec![mi]);
        let mut e = X86Emitter::new(ObjectFormat::Elf);
        let sec = e.emit_function(&mf);
        assert_eq!(sec.data[0], 0x48, "REX.W prefix");
        assert_eq!(sec.data[1], 0x89, "MOV r/m64, r64 opcode");
        assert_eq!(sec.data[2], 0x85, "ModRM(mod=10, reg=RAX=0, rm=RBP=5)");
        assert_eq!(&sec.data[3..7], &[0xF8, 0xFF, 0xFF, 0xFF], "disp32 = -8");
    }

    fn expect_simd_prefix(instr: MInstr, expected: &[u8]) {
        let mf = single_block_mf("simd", vec![instr]);
        let mut e = X86Emitter::new(ObjectFormat::Elf);
        let sec = e.emit_function(&mf);
        assert_eq!(&sec.data[..expected.len()], expected);
    }

    #[test]
    fn paddd_rr_encodes_correctly() {
        expect_simd_prefix(
            MInstr::new(PADDD_RR)
                .with_dst(VReg(RAX.0 as u32))
                .with_preg(RSI),
            &[0x66, 0x0F, 0xFE, 0xC6],
        );
    }

    #[test]
    fn psubd_rr_encodes_correctly() {
        expect_simd_prefix(
            MInstr::new(PSUBD_RR)
                .with_dst(VReg(RAX.0 as u32))
                .with_preg(RSI),
            &[0x66, 0x0F, 0xFA, 0xC6],
        );
    }

    #[test]
    fn pmulld_rr_encodes_correctly() {
        expect_simd_prefix(
            MInstr::new(PMULLD_RR)
                .with_dst(VReg(RAX.0 as u32))
                .with_preg(RSI),
            &[0x66, 0x0F, 0x38, 0x40, 0xC6],
        );
    }

    #[test]
    fn addps_rr_encodes_correctly() {
        expect_simd_prefix(
            MInstr::new(ADDPS_RR)
                .with_dst(VReg(RAX.0 as u32))
                .with_preg(RSI),
            &[0x0F, 0x58, 0xC6],
        );
    }

    #[test]
    fn mulps_rr_encodes_correctly() {
        expect_simd_prefix(
            MInstr::new(MULPS_RR)
                .with_dst(VReg(RAX.0 as u32))
                .with_preg(RSI),
            &[0x0F, 0x59, 0xC6],
        );
    }

    #[test]
    fn divps_rr_encodes_correctly() {
        expect_simd_prefix(
            MInstr::new(DIVPS_RR)
                .with_dst(VReg(RAX.0 as u32))
                .with_preg(RSI),
            &[0x0F, 0x5E, 0xC6],
        );
    }

    #[test]
    fn addpd_rr_encodes_correctly() {
        expect_simd_prefix(
            MInstr::new(ADDPD_RR)
                .with_dst(VReg(RAX.0 as u32))
                .with_preg(RSI),
            &[0x66, 0x0F, 0x58, 0xC6],
        );
    }

    #[test]
    fn mulpd_rr_encodes_correctly() {
        expect_simd_prefix(
            MInstr::new(MULPD_RR)
                .with_dst(VReg(RAX.0 as u32))
                .with_preg(RSI),
            &[0x66, 0x0F, 0x59, 0xC6],
        );
    }

    #[test]
    fn movaps_rr_encodes_correctly() {
        expect_simd_prefix(
            MInstr::new(MOVAPS_RR)
                .with_dst(VReg(RAX.0 as u32))
                .with_preg(RSI),
            &[0x0F, 0x28, 0xC6],
        );
    }

    #[test]
    fn movdqu_load_mr_encodes_correctly() {
        let mi = MInstr {
            opcode: MOVDQU_LOAD_MR,
            dst: Some(VReg(RAX.0 as u32)),
            operands: vec![MOperand::Imm(0)],
            phys_uses: vec![],
            clobbers: vec![],
            debug_loc: None,
        };
        let mf = single_block_mf("simd_ld", vec![mi]);
        let mut e = X86Emitter::new(ObjectFormat::Elf);
        let sec = e.emit_function(&mf);
        assert_eq!(&sec.data[..7], &[0xF3, 0x0F, 0x6F, 0x85, 0xF8, 0xFF, 0xFF]);
    }

    #[test]
    fn movdqu_store_rm_encodes_correctly() {
        let mi = MInstr {
            opcode: MOVDQU_STORE_RM,
            dst: None,
            operands: vec![MOperand::Imm(0), MOperand::PReg(RSI)],
            phys_uses: vec![],
            clobbers: vec![],
            debug_loc: None,
        };
        let mf = single_block_mf("simd_st", vec![mi]);
        let mut e = X86Emitter::new(ObjectFormat::Elf);
        let sec = e.emit_function(&mf);
        assert_eq!(&sec.data[..7], &[0xF3, 0x0F, 0x7F, 0xB5, 0xF8, 0xFF, 0xFF]);
    }

    #[test]
    fn movaps_load_mr_encodes_correctly() {
        let mi = MInstr {
            opcode: MOVAPS_LOAD_MR,
            dst: Some(VReg(RAX.0 as u32)),
            operands: vec![MOperand::Imm(0)],
            phys_uses: vec![],
            clobbers: vec![],
            debug_loc: None,
        };
        let mf = single_block_mf("simd_ld_aligned", vec![mi]);
        let mut e = X86Emitter::new(ObjectFormat::Elf);
        let sec = e.emit_function(&mf);
        assert_eq!(&sec.data[..7], &[0x0F, 0x28, 0x85, 0xF8, 0xFF, 0xFF, 0xFF]);
    }

    #[test]
    fn prologue_emitted_when_frame_size_nonzero() {
        // A MachineFunction with frame_size=8 should emit push rbp; mov rbp,rsp; sub rsp,N.
        let mut mf = MachineFunction::new("framed_fn".into());
        mf.frame_size = 8;
        let b = mf.add_block("entry");
        mf.push(b, MInstr::new(RET));
        let mut e = X86Emitter::new(ObjectFormat::Elf);
        let sec = e.emit_function(&mf);
        // Prologue: 0x55 (push rbp), 0x48 0x89 0xE5 (mov rbp,rsp),
        //           sub rsp,16 (frame_size=8, padded to 16 for n_callee=0 even: needs mult of 16)
        //           = 0x48 0x83 0xEC 0x10
        // Epilogue before RET: add rsp,16 = 0x48 0x83 0xC4 0x10; pop rbp = 0x5D; ret = 0xC3
        assert_eq!(sec.data[0], 0x55, "push rbp");
        assert_eq!(&sec.data[1..4], &[0x48, 0x89, 0xE5], "mov rbp, rsp");
        // sub rsp, 16: 0x48 0x83 0xEC 0x10
        assert_eq!(&sec.data[4..8], &[0x48, 0x83, 0xEC, 0x10], "sub rsp, 16");
        // Epilogue: add rsp,16 = 0x48 0x83 0xC4 0x10; pop rbp = 0x5D
        assert_eq!(&sec.data[8..12], &[0x48, 0x83, 0xC4, 0x10], "add rsp, 16");
        assert_eq!(sec.data[12], 0x5D, "pop rbp");
        assert_eq!(sec.data[13], 0xC3, "ret");
    }

    #[test]
    fn no_prologue_when_frame_size_zero_and_no_callee_saved() {
        // A plain function with no spill slots and no callee-saved usage should
        // emit only the instruction bytes without any prologue/epilogue.
        let mf = single_block_mf("plain_fn", vec![MInstr::new(RET)]);
        let mut e = X86Emitter::new(ObjectFormat::Elf);
        let sec = e.emit_function(&mf);
        // Must be exactly 1 byte: RET = 0xC3.
        assert_eq!(
            sec.data,
            vec![0xC3],
            "no prologue/epilogue for frameless function"
        );
    }

    #[test]
    fn spill_end_to_end_x86() {
        // Build a MachineFunction with more simultaneously live VRegs than
        // allocatable registers to force a spill, run insert_spill_reloads,
        // apply_allocation, and emit — verify the output is non-empty and
        // contains the expected prologue bytes.
        use crate::instructions::{MOV_LOAD_MR, MOV_STORE_RM};
        use llvm_codegen::isel::MOpcode;
        use llvm_codegen::regalloc::{
            allocate_registers, apply_allocation, compute_live_intervals, insert_spill_reloads,
            RegAllocStrategy,
        };

        let mut mf = MachineFunction::new("spill_e2e".into());
        // Only 1 allocatable register to guarantee spills.
        mf.allocatable_pregs = vec![RAX];
        mf.callee_saved_pregs = vec![];
        let b = mf.add_block("entry");
        // v0 = ...
        let v0 = mf.fresh_vreg();
        mf.push(b, MInstr::new(MOpcode(0x10)).with_dst(v0)); // ADD_RR placeholder
                                                             // v1 = ... v0 ...  (v0 and v1 simultaneously live → v0 must spill)
        let v1 = mf.fresh_vreg();
        mf.push(b, MInstr::new(MOpcode(0x10)).with_dst(v1).with_vreg(v0));
        // ret
        mf.push(b, MInstr::new(RET));

        let intervals = compute_live_intervals(&mf);
        let mut result = allocate_registers(
            &intervals,
            &mf.allocatable_pregs,
            &mf.allocatable_fp_pregs,
            RegAllocStrategy::LinearScan,
        );
        assert!(!result.spilled.is_empty(), "must have spills");
        insert_spill_reloads(&mut mf, &mut result, MOV_LOAD_MR, MOV_STORE_RM, MOV_LOAD_MR, MOV_STORE_RM);
        apply_allocation(&mut mf, &result);

        let mut e = X86Emitter::new(ObjectFormat::Elf);
        let sec = e.emit_function(&mf);

        // After spills: frame_size > 0, so prologue must be present.
        assert!(!sec.data.is_empty(), "emitted code must be non-empty");
        assert_eq!(sec.data[0], 0x55, "push rbp must be first byte of prologue");
        assert!(sec.data.contains(&0xC3), "RET must be present in output");
    }

    #[test]
    fn add_sub_imm_fixed_rsp_encoding() {
        let mut mf = MachineFunction::new("stack_adj".into());
        let b0 = mf.add_block("entry");
        mf.push(b0, MInstr::new(SUB_RI).with_preg(RSP).with_imm(32));
        mf.push(b0, MInstr::new(ADD_RI).with_preg(RSP).with_imm(48));
        mf.push(b0, MInstr::new(RET));

        let mut e = X86Emitter::new(ObjectFormat::Elf);
        let sec = e.emit_function(&mf);
        assert!(sec
            .data
            .windows(7)
            .any(|w| w == [0x48, 0x81, 0xEC, 0x20, 0x00, 0x00, 0x00]));
        assert!(sec
            .data
            .windows(7)
            .any(|w| w == [0x48, 0x81, 0xC4, 0x30, 0x00, 0x00, 0x00]));
    }

    #[test]
    fn chkstk_sequence_emitted_for_large_coff_frame() {
        // frame_size = 8192 (> 4096) with COFF format must emit the __chkstk
        // probe sequence: mov rax, 8192 / call __chkstk / sub rsp, rax.
        let mut mf = MachineFunction::new("big_frame".into());
        mf.frame_size = 8192;
        let b0 = mf.add_block("entry");
        mf.push(b0, MInstr::new(RET));

        let mut e = X86Emitter::new(ObjectFormat::Coff);
        let sec = e.emit_function(&mf);

        // sub_rsp = (8192 + 15) & !15 = 8192 = 0x2000
        // Expected sequence in prologue (after push rbp + mov rbp,rsp):
        //   48 B8 00 20 00 00 00 00 00 00   (mov rax, 0x2000)
        //   E8 00 00 00 00                  (call __chkstk, placeholder)
        //   48 2B E0                        (sub rsp, rax)
        let mov_rax: &[u8] = &[0x48, 0xB8, 0x00, 0x20, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        let call_chkstk: &[u8] = &[0xE8, 0x00, 0x00, 0x00, 0x00];
        let sub_rsp_rax: &[u8] = &[0x48, 0x2B, 0xE0];

        assert!(
            sec.data.windows(mov_rax.len()).any(|w| w == mov_rax),
            "missing: mov rax, 8192"
        );
        assert!(
            sec.data.windows(call_chkstk.len()).any(|w| w == call_chkstk),
            "missing: call __chkstk placeholder"
        );
        assert!(
            sec.data.windows(sub_rsp_rax.len()).any(|w| w == sub_rsp_rax),
            "missing: sub rsp, rax"
        );
        // Must have a reloc for the __chkstk call
        assert!(
            sec.relocs.iter().any(|r| r.external_name.as_deref() == Some("__chkstk")),
            "missing __chkstk reloc"
        );
    }

    #[test]
    fn coff_large_frame_emits_external_chkstk_symbol() {
        // emit_object must insert an undefined Symbol for __chkstk when the
        // prologue emits a call to it.
        let mut mf = MachineFunction::new("big_frame2".into());
        mf.frame_size = 8192;
        let b0 = mf.add_block("entry");
        mf.push(b0, MInstr::new(RET));

        let mut e = X86Emitter::new(ObjectFormat::Coff);
        let obj = emit_object(&mf, &mut e);

        let chkstk_sym = obj.symbols.iter().find(|s| s.name == "__chkstk");
        assert!(chkstk_sym.is_some(), "no __chkstk symbol in COFF object");
        assert!(chkstk_sym.unwrap().undefined, "__chkstk must be undefined");
    }

    #[test]
    fn elf_large_frame_does_not_emit_chkstk() {
        // __chkstk is Win64-only; ELF large frames must use regular sub rsp, imm32.
        let mut mf = MachineFunction::new("big_frame_elf".into());
        mf.frame_size = 8192;
        let b0 = mf.add_block("entry");
        mf.push(b0, MInstr::new(RET));

        let mut e = X86Emitter::new(ObjectFormat::Elf);
        let sec = e.emit_function(&mf);

        // sub rsp, 8192 (imm32 form): 48 81 EC 00 20 00 00
        let sub_rsp_imm32: &[u8] = &[0x48, 0x81, 0xEC, 0x00, 0x20, 0x00, 0x00];
        assert!(
            sec.data.windows(sub_rsp_imm32.len()).any(|w| w == sub_rsp_imm32),
            "ELF large frame must use sub rsp, imm32 (not __chkstk)"
        );
        assert!(
            !sec.relocs.iter().any(|r| r.external_name.as_deref() == Some("__chkstk")),
            "ELF must not emit __chkstk reloc"
        );
    }

    // ── atomic encoding tests ─────────────────────────────────────────────

    #[test]
    fn mfence_encodes_correctly() {
        let mf = single_block_mf("mfence_fn", vec![MInstr::new(MFENCE)]);
        let mut e = X86Emitter::new(ObjectFormat::Elf);
        let sec = e.emit_function(&mf);
        assert_eq!(sec.data, vec![0x0F, 0xAE, 0xF0]);
    }

    #[test]
    fn lock_cmpxchg_mr_rcx_rdx_encodes_correctly() {
        // LOCK CMPXCHG [RCX], RDX  (ptr=RCX=1, comparand=RAX=0, new_val=RDX=2)
        // Expected: F0 (LOCK) + REX.W(0x48) + 0F B1 + ModRM(00_010_001=0x11)
        use crate::regs::{RCX, RDX};
        let mi = MInstr {
            opcode: LOCK_CMPXCHG_MR,
            dst: None,
            operands: vec![MOperand::PReg(RCX), MOperand::PReg(RAX), MOperand::PReg(RDX)],
            phys_uses: vec![],
            clobbers: vec![],
            debug_loc: None,
        };
        let mf = single_block_mf("cmpxchg_fn", vec![mi]);
        let mut e = X86Emitter::new(ObjectFormat::Elf);
        let sec = e.emit_function(&mf);
        // F0 LOCK, 48 REX.W, 0F B1, ModRM(mod=00, reg=RDX=2, rm=RCX=1) = 0x11
        assert_eq!(sec.data[0], 0xF0, "LOCK prefix");
        assert_eq!(sec.data[1], 0x48, "REX.W");
        assert_eq!(sec.data[2], 0x0F, "escape byte");
        assert_eq!(sec.data[3], 0xB1, "CMPXCHG opcode");
        assert_eq!(sec.data[4], 0x11, "ModRM(00 010 001): mod=00 reg=RDX rm=RCX");
    }

    #[test]
    fn lock_xadd_mr_rdi_rsi_encodes_correctly() {
        // LOCK XADD [RDI], RSI  (ptr=RDI=7, val=RSI=6)
        // Expected: F0 + REX.W(0x48) + 0F C1 + ModRM(00_110_111=0x37)
        use crate::regs::{RDI, RSI};
        let mi = MInstr {
            opcode: LOCK_XADD_MR,
            dst: None,
            operands: vec![MOperand::PReg(RDI), MOperand::PReg(RSI)],
            phys_uses: vec![],
            clobbers: vec![],
            debug_loc: None,
        };
        let mf = single_block_mf("xadd_fn", vec![mi]);
        let mut e = X86Emitter::new(ObjectFormat::Elf);
        let sec = e.emit_function(&mf);
        // F0, 48, 0F, C1, ModRM(00 110 111) = 0x37
        assert_eq!(sec.data[0], 0xF0, "LOCK prefix");
        assert_eq!(sec.data[1], 0x48, "REX.W");
        assert_eq!(sec.data[2], 0x0F);
        assert_eq!(sec.data[3], 0xC1, "XADD opcode");
        assert_eq!(sec.data[4], 0x37, "ModRM(00 110 111): mod=00 reg=RSI rm=RDI");
    }

    #[test]
    fn xchg_mr_rax_rdi_encodes_correctly() {
        // XCHG [RDI], RAX  (ptr=RDI=7, val=RAX=0)
        // Expected: REX.W(0x48) + 0x87 + ModRM(00_000_111=0x07)
        let mi = MInstr {
            opcode: XCHG_MR,
            dst: None,
            operands: vec![MOperand::PReg(RDI), MOperand::PReg(RAX)],
            phys_uses: vec![],
            clobbers: vec![],
            debug_loc: None,
        };
        let mf = single_block_mf("xchg_fn", vec![mi]);
        let mut e = X86Emitter::new(ObjectFormat::Elf);
        let sec = e.emit_function(&mf);
        // 48, 87, ModRM(00 000 111) = 0x07
        assert_eq!(sec.data[0], 0x48, "REX.W");
        assert_eq!(sec.data[1], 0x87, "XCHG opcode");
        assert_eq!(sec.data[2], 0x07, "ModRM(00 000 111): mod=00 reg=RAX rm=RDI");
    }

    #[test]
    fn lock_and_mr_rsi_rdi_encodes_correctly() {
        // LOCK AND [RDI], RSI  (ptr=RDI=7, val=RSI=6)
        // Expected: F0 + REX.W(0x48) + 0x21 + ModRM(00_110_111=0x37)
        let mi = MInstr {
            opcode: LOCK_AND_MR,
            dst: None,
            operands: vec![MOperand::PReg(RDI), MOperand::PReg(RSI)],
            phys_uses: vec![],
            clobbers: vec![],
            debug_loc: None,
        };
        let mf = single_block_mf("lock_and_fn", vec![mi]);
        let mut e = X86Emitter::new(ObjectFormat::Elf);
        let sec = e.emit_function(&mf);
        assert_eq!(sec.data[0], 0xF0, "LOCK prefix");
        assert_eq!(sec.data[1], 0x48, "REX.W");
        assert_eq!(sec.data[2], 0x21, "AND r/m64,r64 opcode");
        assert_eq!(sec.data[3], 0x37, "ModRM(00 110 111)");
    }

    #[test]
    fn lock_or_mr_encodes_correctly() {
        // LOCK OR [RDI], RSI
        let mi = MInstr {
            opcode: LOCK_OR_MR,
            dst: None,
            operands: vec![MOperand::PReg(RDI), MOperand::PReg(RSI)],
            phys_uses: vec![],
            clobbers: vec![],
            debug_loc: None,
        };
        let mf = single_block_mf("lock_or_fn", vec![mi]);
        let mut e = X86Emitter::new(ObjectFormat::Elf);
        let sec = e.emit_function(&mf);
        assert_eq!(sec.data[0], 0xF0, "LOCK prefix");
        assert_eq!(sec.data[1], 0x48, "REX.W");
        assert_eq!(sec.data[2], 0x09, "OR r/m64,r64 opcode");
        assert_eq!(sec.data[3], 0x37, "ModRM(00 110 111)");
    }

    #[test]
    fn lock_xor_mr_encodes_correctly() {
        // LOCK XOR [RDI], RSI
        let mi = MInstr {
            opcode: LOCK_XOR_MR,
            dst: None,
            operands: vec![MOperand::PReg(RDI), MOperand::PReg(RSI)],
            phys_uses: vec![],
            clobbers: vec![],
            debug_loc: None,
        };
        let mf = single_block_mf("lock_xor_fn", vec![mi]);
        let mut e = X86Emitter::new(ObjectFormat::Elf);
        let sec = e.emit_function(&mf);
        assert_eq!(sec.data[0], 0xF0, "LOCK prefix");
        assert_eq!(sec.data[1], 0x48, "REX.W");
        assert_eq!(sec.data[2], 0x31, "XOR r/m64,r64 opcode");
        assert_eq!(sec.data[3], 0x37, "ModRM(00 110 111)");
    }

    #[test]
    fn modrm_mem_helper_rsp_edge_case() {
        // When ptr_reg=RSP (enc=4), ModRM rm=4 means SIB follows.
        // LOCK AND [RSP], RSI must emit SIB=0x24 after ModRM.
        let mi = MInstr {
            opcode: LOCK_AND_MR,
            dst: None,
            operands: vec![MOperand::PReg(RSP), MOperand::PReg(RSI)],
            phys_uses: vec![],
            clobbers: vec![],
            debug_loc: None,
        };
        let mf = single_block_mf("lock_and_rsp_fn", vec![mi]);
        let mut e = X86Emitter::new(ObjectFormat::Elf);
        let sec = e.emit_function(&mf);
        // F0 LOCK, 48 REX.W, 21 AND, ModRM(00 110 100=0x34), SIB(0x24)
        assert_eq!(sec.data[0], 0xF0, "LOCK prefix");
        assert_eq!(sec.data[1], 0x48, "REX.W");
        assert_eq!(sec.data[2], 0x21, "AND opcode");
        assert_eq!(sec.data[3], 0x34, "ModRM(00 110 100): reg=RSI rm=RSP");
        assert_eq!(sec.data[4], 0x24, "SIB(00 100 100) for [RSP]");
    }

    #[test]
    fn modrm_mem_helper_rbp_edge_case() {
        // When ptr_reg=RBP (enc=5), mod=00 rm=5 means [RIP+disp32].
        // Must use mod=01 disp8=0 instead.
        use crate::regs::RBP;
        let mi = MInstr {
            opcode: LOCK_AND_MR,
            dst: None,
            operands: vec![MOperand::PReg(RBP), MOperand::PReg(RSI)],
            phys_uses: vec![],
            clobbers: vec![],
            debug_loc: None,
        };
        let mf = single_block_mf("lock_and_rbp_fn", vec![mi]);
        let mut e = X86Emitter::new(ObjectFormat::Elf);
        let sec = e.emit_function(&mf);
        // F0 LOCK, 48 REX.W, 21 AND, ModRM(01 110 101=0x75), disp8=0x00
        assert_eq!(sec.data[0], 0xF0, "LOCK prefix");
        assert_eq!(sec.data[1], 0x48, "REX.W");
        assert_eq!(sec.data[2], 0x21, "AND opcode");
        assert_eq!(sec.data[3], 0x75, "ModRM(01 110 101): mod=01 reg=RSI rm=RBP");
        assert_eq!(sec.data[4], 0x00, "disp8=0 for mod=01 RBP base");
    }

    // ── non-promotable alloca frame-slot encoding tests ───────────────────────

    #[test]
    fn lea_frame_mr_slot0_rax() {
        // LEA_FRAME_MR: lea rax, [rbp - 8]  (slot 0, no callee-saved regs)
        // disp = -(callee_save_bytes + (slot_idx+1)*8) = -(0 + 8) = -8
        // Encoding: REX.W(0x48) + LEA(0x8D) + ModRM(mod=10, reg=RAX=0, rm=RBP=5 → 0x85)
        //           + disp32(-8) = F8 FF FF FF
        let mi = MInstr {
            opcode: LEA_FRAME_MR,
            dst: Some(VReg(RAX.0 as u32)),
            operands: vec![MOperand::Imm(0)], // slot_idx = 0
            phys_uses: vec![],
            clobbers: vec![],
            debug_loc: None,
        };
        let mf = single_block_mf("lea_frame_fn", vec![mi]);
        let mut e = X86Emitter::new(ObjectFormat::Elf);
        let sec = e.emit_function(&mf);
        assert_eq!(sec.data[0], 0x48, "REX.W prefix");
        assert_eq!(sec.data[1], 0x8D, "LEA opcode");
        assert_eq!(sec.data[2], 0x85, "ModRM(mod=10, reg=RAX=0, rm=RBP=5)");
        assert_eq!(&sec.data[3..7], &[0xF8, 0xFF, 0xFF, 0xFF], "disp32 = -8");
    }

    #[test]
    fn lea_frame_mr_slot1_rax() {
        // LEA_FRAME_MR: lea rax, [rbp - 16]  (slot 1, no callee-saved regs)
        // disp = -(0 + (1+1)*8) = -16 = 0xFFFFFFF0
        let mi = MInstr {
            opcode: LEA_FRAME_MR,
            dst: Some(VReg(RAX.0 as u32)),
            operands: vec![MOperand::Imm(1)], // slot_idx = 1
            phys_uses: vec![],
            clobbers: vec![],
            debug_loc: None,
        };
        let mf = single_block_mf("lea_frame_slot1_fn", vec![mi]);
        let mut e = X86Emitter::new(ObjectFormat::Elf);
        let sec = e.emit_function(&mf);
        assert_eq!(sec.data[0], 0x48, "REX.W prefix");
        assert_eq!(sec.data[1], 0x8D, "LEA opcode");
        assert_eq!(sec.data[2], 0x85, "ModRM(mod=10, reg=RAX=0, rm=RBP=5)");
        assert_eq!(&sec.data[3..7], &[0xF0, 0xFF, 0xFF, 0xFF], "disp32 = -16");
    }

    #[test]
    fn mov_load_reg_mr_through_rdi_into_rax() {
        // MOV_LOAD_REG_MR: mov rax, [rdi]  — load from pointer in RDI into RAX
        // Encoding: REX.W(0x48) + 0x8B + ModRM(mod=00, reg=RAX=0, rm=RDI=7 → 0x07)
        let mi = MInstr {
            opcode: MOV_LOAD_REG_MR,
            dst: Some(VReg(RAX.0 as u32)),
            operands: vec![MOperand::PReg(RDI)],
            phys_uses: vec![],
            clobbers: vec![],
            debug_loc: None,
        };
        let mf = single_block_mf("load_reg_fn", vec![mi]);
        let mut e = X86Emitter::new(ObjectFormat::Elf);
        let sec = e.emit_function(&mf);
        // REX.W=0x48, MOV r64,r/m64=0x8B, ModRM(mod=00, reg=RAX=0, rm=RDI=7)=0x07
        assert_eq!(sec.data[0], 0x48, "REX.W prefix");
        assert_eq!(sec.data[1], 0x8B, "MOV r64, r/m64 opcode");
        assert_eq!(sec.data[2], 0x07, "ModRM(mod=00, reg=RAX=0, rm=RDI=7)");
    }

    #[test]
    fn mov_store_reg_rm_through_rdi_from_rsi() {
        // MOV_STORE_REG_RM: mov [rdi], rsi  — store RSI through pointer in RDI
        // Encoding: REX.W(0x48) + 0x89 + ModRM(mod=00, reg=RSI=6, rm=RDI=7 → 0x37)
        let mi = MInstr {
            opcode: MOV_STORE_REG_RM,
            dst: None,
            operands: vec![MOperand::PReg(RDI), MOperand::PReg(RSI)],
            phys_uses: vec![],
            clobbers: vec![],
            debug_loc: None,
        };
        let mf = single_block_mf("store_reg_fn", vec![mi]);
        let mut e = X86Emitter::new(ObjectFormat::Elf);
        let sec = e.emit_function(&mf);
        // REX.W=0x48, MOV r/m64,r64=0x89, ModRM(mod=00, reg=RSI=6, rm=RDI=7)=0x37
        assert_eq!(sec.data[0], 0x48, "REX.W prefix");
        assert_eq!(sec.data[1], 0x89, "MOV r/m64, r64 opcode");
        assert_eq!(sec.data[2], 0x37, "ModRM(mod=00, reg=RSI=6, rm=RDI=7)");
    }

    #[test]
    fn lea_frame_mr_shifts_spill_slots_when_alloca_bytes_nonzero() {
        // When alloca_frame_bytes > 0, spill slots (MOV_LOAD_MR) must be shifted
        // below the alloca area.  Alloca slot 0: [rbp-8], spill slot 0: [rbp-16].
        use crate::instructions::{LEA_FRAME_MR, MOV_LOAD_MR};
        let lea_mi = MInstr {
            opcode: LEA_FRAME_MR,
            dst: Some(VReg(RAX.0 as u32)),
            operands: vec![MOperand::Imm(0)],
            phys_uses: vec![],
            clobbers: vec![],
            debug_loc: None,
        };
        let load_mi = MInstr {
            opcode: MOV_LOAD_MR,
            dst: Some(VReg(RAX.0 as u32)),
            operands: vec![MOperand::Imm(0)], // spill slot 0
            phys_uses: vec![],
            clobbers: vec![],
            debug_loc: None,
        };
        let mut mf = MachineFunction::new("shift_fn".into());
        let b = mf.add_block("entry");
        mf.push(b, lea_mi);
        mf.push(b, load_mi);
        // Simulate one alloca slot (8 bytes) having been allocated during lowering.
        mf.alloca_frame_bytes = 8;

        let mut e = X86Emitter::new(ObjectFormat::Elf);
        let sec = e.emit_function(&mf);

        // The function has alloca_frame_bytes=8 and no callee-saved regs, so
        // needs_frame=true. Prologue: push rbp(55) + mov rbp,rsp(48 89 E5) + sub rsp,16(48 83 EC 10).
        // After prologue (7 bytes), LEA_FRAME_MR(slot=0) at offset 7:
        //   disp = -(0 + (0+1)*8) = -8 → [F8 FF FF FF]
        // MOV_LOAD_MR(slot=0) at offset 14:
        //   disp = -(0 + 8 + (0+1)*8) = -16 → [F0 FF FF FF]

        // Find LEA in output by opcode bytes (48 8D 85 ...)
        let pos = sec
            .data
            .windows(2)
            .position(|w| w == [0x8D, 0x85])
            .expect("LEA (8D 85) not found");
        let lea_disp = i32::from_le_bytes([
            sec.data[pos + 2],
            sec.data[pos + 3],
            sec.data[pos + 4],
            sec.data[pos + 5],
        ]);
        assert_eq!(lea_disp, -8, "alloca slot 0 displacement should be -8");

        // Find MOV_LOAD (8B 85) in output
        let pos2 = sec
            .data
            .windows(2)
            .position(|w| w == [0x8B, 0x85])
            .expect("MOV_LOAD (8B 85) not found");
        let load_disp = i32::from_le_bytes([
            sec.data[pos2 + 2],
            sec.data[pos2 + 3],
            sec.data[pos2 + 4],
            sec.data[pos2 + 5],
        ]);
        assert_eq!(
            load_disp, -16,
            "spill slot 0 must be shifted below alloca area"
        );
    }

    #[test]
    fn alloca_frame_bytes_included_in_sub_rsp() {
        // When alloca_frame_bytes > 0 and frame_size = 0, the SUB RSP must still
        // reserve space.  A single 8-byte alloca slot should produce sub rsp, 16
        // (aligned to 16 bytes: 8 bytes raw, rounded up to 16 for alignment).
        let mi = MInstr::new(NOP); // placeholder instruction
        let mut mf = MachineFunction::new("alloca_sub_fn".into());
        mf.alloca_frame_bytes = 8; // one 8-byte alloca slot
        let b = mf.add_block("entry");
        mf.push(b, mi);

        let mut e = X86Emitter::new(ObjectFormat::Elf);
        let sec = e.emit_function(&mf);

        // Prologue: push rbp (55), mov rbp,rsp (48 89 E5), sub rsp, N (48 83 EC N).
        // N = aligned((0+8), 16) = 16.
        assert_eq!(sec.data[0], 0x55, "push rbp");
        assert_eq!(&sec.data[1..4], &[0x48, 0x89, 0xE5], "mov rbp, rsp");
        // sub rsp, 16: 0x48 0x83 0xEC 0x10
        assert_eq!(sec.data[4], 0x48, "REX.W for sub rsp");
        assert_eq!(sec.data[5], 0x83, "sub r/m64, imm8 opcode");
        assert_eq!(sec.data[6], 0xEC, "ModRM /5 RSP");
        assert_eq!(sec.data[7], 16, "sub rsp, 16 (aligned alloca space)");
    }
}
