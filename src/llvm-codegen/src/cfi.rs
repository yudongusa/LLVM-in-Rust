//! DWARF CFI (.eh_frame) emission for x86-64 ELF stack unwinding.
//!
//! Produces a well-formed `.eh_frame` section containing:
//! - One **CIE** (Common Information Entry) with `zR` augmentation encoding
//!   FDE pc-begin pointers as PC-relative sdata4 values.
//! - One **FDE** (Frame Description Entry) per function, describing the
//!   call-frame state changes during the prologue.
//!
//! DWARF x86-64 register numbers used here:
//!   RAX=0, RDX=1, RCX=2, RBX=3, RSI=4, RDI=5, RBP=6, RSP=7, RIP=16

use crate::dwarf_vars::{encode_sleb128, encode_uleb128};

// ── CFI Instruction ────────────────────────────────────────────────────────

/// A single DWARF Call Frame Instruction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CfiInstr {
    /// `DW_CFA_nop` (0x00) — no-operation / padding byte.
    Nop,
    /// `DW_CFA_advance_loc(delta)` (0x40 | delta) — advance the location
    /// counter by `delta` code units (1 unit = 1 byte on x86-64).
    AdvanceLoc(u8),
    /// `DW_CFA_def_cfa(reg, offset)` (0x0c) — CFA = register + offset.
    DefCfa { reg: u8, offset: u32 },
    /// `DW_CFA_def_cfa_register(reg)` (0x0d) — switch CFA register, keep offset.
    DefCfaRegister(u8),
    /// `DW_CFA_def_cfa_offset(offset)` (0x0e) — update CFA offset, keep register.
    DefCfaOffset(u32),
    /// `DW_CFA_offset(reg, factored_offset)` (0x80 | reg) — register saved at
    /// `CFA + factored_offset * data_alignment_factor`.
    /// For x86-64 (data_align = -8), `factored_offset = |byte_offset| / 8`.
    Offset { reg: u8, factored_offset: u32 },
}

impl CfiInstr {
    /// Encode the instruction to raw bytes.
    pub fn encode(&self) -> Vec<u8> {
        match self {
            CfiInstr::Nop => vec![0x00],
            CfiInstr::AdvanceLoc(delta) => vec![0x40 | (delta & 0x3f)],
            CfiInstr::DefCfa { reg, offset } => {
                let mut v = vec![0x0c];
                v.extend(encode_uleb128(*reg as u64));
                v.extend(encode_uleb128(*offset as u64));
                v
            }
            CfiInstr::DefCfaRegister(reg) => {
                let mut v = vec![0x0d];
                v.extend(encode_uleb128(*reg as u64));
                v
            }
            CfiInstr::DefCfaOffset(offset) => {
                let mut v = vec![0x0e];
                v.extend(encode_uleb128(*offset as u64));
                v
            }
            CfiInstr::Offset { reg, factored_offset } => {
                // DW_CFA_offset: high 2 bits = 0b10, low 6 bits = reg
                let mut v = vec![0x80 | (reg & 0x3f)];
                v.extend(encode_uleb128(*factored_offset as u64));
                v
            }
        }
    }
}

// ── CfiWriter ─────────────────────────────────────────────────────────────

/// Accumulates a `.eh_frame` section: one CIE followed by per-function FDEs.
///
/// Usage:
/// ```rust,ignore
/// let mut cfi = CfiWriter::new();
/// cfi.write_cie();
/// cfi.write_fde(fn_size_bytes, &prologue_instrs);
/// let section_data = cfi.assemble();
/// ```
pub struct CfiWriter {
    /// Raw bytes of the CIE record (including its length prefix).
    pub cie_bytes: Vec<u8>,
    /// Raw bytes of each FDE record (including its length prefix).
    pub fdes: Vec<Vec<u8>>,
}

impl CfiWriter {
    /// Create a new, empty `CfiWriter`.
    pub fn new() -> Self {
        Self {
            cie_bytes: Vec::new(),
            fdes: Vec::new(),
        }
    }

    /// Emit the CIE (Common Information Entry).
    ///
    /// CIE shape (for x86-64 ELF, augmentation `zR`):
    /// ```text
    /// u32  length            (bytes after this field)
    /// u32  CIE_id = 0
    /// u8   version = 1
    /// "zR\0" augmentation string
    /// ULEB128  code_alignment_factor = 1
    /// SLEB128  data_alignment_factor = -8
    /// ULEB128  return_address_column = 16 (RIP)
    /// ULEB128  augmentation_data_len = 1
    /// u8       FDE pointer encoding = 0x1b (DW_EH_PE_pcrel | DW_EH_PE_sdata4)
    /// CFI initial_instructions:
    ///   DW_CFA_def_cfa(rsp=7, 8)   — at entry CFA = RSP+8
    ///   DW_CFA_offset(rip=16, 1)   — return addr at CFA-8 (factored: 8/8=1)
    ///   DW_CFA_nop padding to 8-byte boundary
    /// ```
    pub fn write_cie(&mut self) {
        let mut body: Vec<u8> = Vec::new();
        // CIE_id = 0
        body.extend_from_slice(&0u32.to_le_bytes());
        // version
        body.push(1);
        // augmentation string "zR\0"
        body.extend_from_slice(b"zR\0");
        // code alignment factor = 1
        body.extend(encode_uleb128(1));
        // data alignment factor = -8
        body.extend(encode_sleb128(-8));
        // return address register = 16 (RIP)
        body.extend(encode_uleb128(16));
        // augmentation data length = 1
        body.extend(encode_uleb128(1));
        // FDE pointer encoding: DW_EH_PE_pcrel | DW_EH_PE_sdata4 = 0x10 | 0x0b = 0x1b
        body.push(0x1b);

        // Initial CFI instructions.
        // DW_CFA_def_cfa(RSP=7, 8): at function entry CFA = RSP+8
        body.extend(CfiInstr::DefCfa { reg: 7, offset: 8 }.encode());
        // DW_CFA_offset(RIP=16, 1): return address saved at CFA-8 (factored offset 8/8=1)
        body.extend(CfiInstr::Offset { reg: 16, factored_offset: 1 }.encode());

        // Pad to 8-byte boundary (length field + body must be 8-byte aligned).
        // The total record = 4 (length) + body.len(); pad body so total is multiple of 8.
        while (4 + body.len()) % 8 != 0 {
            body.extend(CfiInstr::Nop.encode());
        }

        // Prepend the 4-byte length field (bytes after the length field itself).
        let mut out: Vec<u8> = (body.len() as u32).to_le_bytes().to_vec();
        out.extend(body);
        self.cie_bytes = out;
    }

    /// Emit one FDE (Frame Description Entry) for a single function.
    ///
    /// `func_size_bytes` — the byte length of the function's machine code.
    /// `prologue_cfi`    — CFI instructions describing the function's prologue.
    ///
    /// The `pc_begin` (initial location) field is written as 0 with a
    /// `// TODO: add R_X86_64_PC32 relocation` marker; callers that need live
    /// relocations should add a `Reloc { kind: RelocKind::Pc32, addend: -4 }`
    /// pointing at the `pc_begin` field offset within the assembled `.eh_frame`.
    pub fn write_fde(&mut self, func_size_bytes: u32, prologue_cfi: &[CfiInstr]) {
        let mut body: Vec<u8> = Vec::new();

        // CIE_pointer: byte offset from this field back to the CIE.
        // After assembly the FDE will start at offset `cie_bytes.len()` (+ earlier FDEs).
        // This is a forward-reference placeholder; assemble() fills it in correctly.
        // We use 0 here and fix it up in assemble().
        body.extend_from_slice(&0u32.to_le_bytes()); // CIE_pointer placeholder

        // pc_begin: PC-relative i32 pointing to function start.
        // TODO: add R_X86_64_PC32 relocation from the linker perspective.
        body.extend_from_slice(&0i32.to_le_bytes()); // pc_begin placeholder

        // pc_range: length of the function in bytes.
        body.extend_from_slice(&func_size_bytes.to_le_bytes());

        // augmentation_data_len = 0 (no extra augmentation data in FDE).
        body.extend(encode_uleb128(0));

        // Encode the prologue CFI instructions.
        for instr in prologue_cfi {
            body.extend(instr.encode());
        }

        // Pad to 4-byte boundary.
        while (4 + body.len()) % 4 != 0 {
            body.extend(CfiInstr::Nop.encode());
        }

        // Prepend length field.
        let mut out: Vec<u8> = (body.len() as u32).to_le_bytes().to_vec();
        out.extend(body);
        self.fdes.push(out);
    }

    /// Assemble all records into the final `.eh_frame` byte sequence:
    /// `CIE || FDE₀ || FDE₁ || ... || terminator(0x00000000)`.
    ///
    /// The `CIE_pointer` field of each FDE is fixed up to point back to the
    /// CIE at byte offset 0.
    pub fn assemble(&self) -> Vec<u8> {
        let mut out = self.cie_bytes.clone();

        for fde_raw in &self.fdes {
            // The FDE starts at `out.len()`.  Its CIE_pointer field is at
            // bytes [4..8] of fde_raw (right after the 4-byte length field).
            // CIE_pointer = byte offset from *this field* back to the CIE,
            // expressed as a positive offset: position_of_field - cie_offset.
            // CIE is at offset 0, this field is at `out.len() + 4`.
            let cie_ptr_field_pos = out.len() + 4; // position within output where CIE_ptr lives
            let cie_ptr = cie_ptr_field_pos as u32; // distance back to CIE at offset 0

            let mut fde = fde_raw.clone();
            // Fix up CIE_pointer at bytes [4..8].
            let ptr_bytes = cie_ptr.to_le_bytes();
            fde[4] = ptr_bytes[0];
            fde[5] = ptr_bytes[1];
            fde[6] = ptr_bytes[2];
            fde[7] = ptr_bytes[3];

            out.extend(fde);
        }

        // .eh_frame terminator: 4-byte zero length record.
        out.extend_from_slice(&0u32.to_le_bytes());
        out
    }

    /// Encode a ULEB128 value (delegates to `dwarf_vars::encode_uleb128`).
    pub fn encode_uleb128(val: u64) -> Vec<u8> {
        encode_uleb128(val)
    }

    /// Encode a SLEB128 value (delegates to `dwarf_vars::encode_sleb128`).
    pub fn encode_sleb128(val: i64) -> Vec<u8> {
        encode_sleb128(val)
    }
}

impl Default for CfiWriter {
    fn default() -> Self {
        Self::new()
    }
}

/// Build a standard x86-64 frame-pointer prologue CFI sequence.
///
/// Covers the two-instruction prologue:
/// - `push rbp`      (1 byte)  — saves old RBP, CFA offset becomes 16
/// - `mov rbp, rsp`  (3 bytes) — CFA register switches to RBP
///
/// Returns the sequence to pass to [`CfiWriter::write_fde`].
pub fn x86_fp_prologue_cfi() -> Vec<CfiInstr> {
    vec![
        // After "push rbp" (1 byte), RSP decreases by 8 → CFA = RSP+16.
        CfiInstr::AdvanceLoc(1),
        CfiInstr::DefCfaOffset(16),
        // RBP was saved at CFA-16 (factored: 16/8 = 2).
        CfiInstr::Offset { reg: 6, factored_offset: 2 },
        // After "mov rbp, rsp" (3 bytes), CFA register switches to RBP.
        CfiInstr::AdvanceLoc(3),
        CfiInstr::DefCfaRegister(6),
    ]
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── CfiInstr encoding ──────────────────────────────────────────────────

    #[test]
    fn cfi_nop_encoding() {
        assert_eq!(CfiInstr::Nop.encode(), vec![0x00]);
    }

    #[test]
    fn cfi_advance_loc_encoding() {
        // DW_CFA_advance_loc(3) = 0x40 | 3 = 0x43
        assert_eq!(CfiInstr::AdvanceLoc(3).encode(), vec![0x43]);
    }

    #[test]
    fn cfi_advance_loc_one() {
        // DW_CFA_advance_loc(1) = 0x41
        assert_eq!(CfiInstr::AdvanceLoc(1).encode(), vec![0x41]);
    }

    #[test]
    fn cfi_def_cfa_encoding() {
        // DW_CFA_def_cfa(reg=7, offset=8) = [0x0c, 0x07, 0x08]
        assert_eq!(
            CfiInstr::DefCfa { reg: 7, offset: 8 }.encode(),
            vec![0x0c, 0x07, 0x08]
        );
    }

    #[test]
    fn cfi_def_cfa_register() {
        // DW_CFA_def_cfa_register(6) = [0x0d, 0x06]
        assert_eq!(CfiInstr::DefCfaRegister(6).encode(), vec![0x0d, 0x06]);
    }

    #[test]
    fn cfi_def_cfa_offset() {
        // DW_CFA_def_cfa_offset(16) = [0x0e, 0x10]
        assert_eq!(CfiInstr::DefCfaOffset(16).encode(), vec![0x0e, 0x10]);
    }

    #[test]
    fn cfi_offset_reg6_factor2() {
        // DW_CFA_offset(reg=6, factored_offset=2): 0x80 | 6 = 0x86, then ULEB128(2)=0x02
        assert_eq!(
            CfiInstr::Offset { reg: 6, factored_offset: 2 }.encode(),
            vec![0x86, 0x02]
        );
    }

    #[test]
    fn cfi_offset_rip_factor1() {
        // DW_CFA_offset(reg=16, factored_offset=1): 0x80 | 16 = 0x90, ULEB128(1)=0x01
        assert_eq!(
            CfiInstr::Offset { reg: 16, factored_offset: 1 }.encode(),
            vec![0x90, 0x01]
        );
    }

    // ── CfiWriter / CIE structure ──────────────────────────────────────────

    #[test]
    fn cie_starts_with_zero_cie_id() {
        let mut cfi = CfiWriter::new();
        cfi.write_cie();
        let cie = &cfi.cie_bytes;
        // First 4 bytes: length field.  Next 4 bytes: CIE_id must be 0.
        assert!(cie.len() >= 8, "CIE must be at least 8 bytes");
        let cie_id = u32::from_le_bytes([cie[4], cie[5], cie[6], cie[7]]);
        assert_eq!(cie_id, 0, "CIE_id must be zero");
    }

    #[test]
    fn cie_has_zr_augmentation() {
        let mut cfi = CfiWriter::new();
        cfi.write_cie();
        let cie = &cfi.cie_bytes;
        // "zR\0" must appear somewhere in the CIE bytes.
        let found = cie.windows(3).any(|w| w == b"zR\0");
        assert!(found, "CIE must contain augmentation string 'zR\\0'");
    }

    #[test]
    fn cie_version_is_one() {
        let mut cfi = CfiWriter::new();
        cfi.write_cie();
        let cie = &cfi.cie_bytes;
        // Byte 8 (after 4-byte length + 4-byte CIE_id) is the version field.
        assert!(cie.len() > 8, "CIE too short");
        assert_eq!(cie[8], 1, "CIE version must be 1");
    }

    #[test]
    fn cie_is_8_byte_aligned() {
        let mut cfi = CfiWriter::new();
        cfi.write_cie();
        // The total CIE record (including length field) must be 8-byte aligned.
        assert_eq!(
            cfi.cie_bytes.len() % 8,
            0,
            "CIE total size must be 8-byte aligned"
        );
    }

    #[test]
    fn cie_fde_pointer_encoding_is_0x1b() {
        let mut cfi = CfiWriter::new();
        cfi.write_cie();
        // The FDE pointer encoding byte (DW_EH_PE_pcrel | DW_EH_PE_sdata4 = 0x1b)
        // must appear somewhere in the CIE bytes.
        assert!(
            cfi.cie_bytes.contains(&0x1b),
            "CIE must encode FDE pointer as 0x1b (pcrel|sdata4)"
        );
    }

    // ── assemble() / CIE + FDE layout ─────────────────────────────────────

    #[test]
    fn assemble_contains_cie_then_fde() {
        let mut cfi = CfiWriter::new();
        cfi.write_cie();
        cfi.write_fde(42, &[]);

        let bytes = cfi.assemble();

        // CIE is at offset 0; its length is encoded in bytes [0..4].
        assert!(bytes.len() > 4, "assembled output must be non-trivial");
        let cie_len = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
        // The CIE record occupies 4 (length field) + cie_len bytes.
        let cie_total = 4 + cie_len;
        assert!(bytes.len() > cie_total, "output must contain more than the CIE");

        // The FDE starts at cie_total.
        let fde_off = cie_total;
        // FDE's CIE_pointer field is at fde_off+4 and must point back to the CIE at 0.
        // CIE_pointer = distance from the CIE_pointer field itself to the CIE = fde_off + 4.
        let cie_ptr = u32::from_le_bytes([
            bytes[fde_off + 4],
            bytes[fde_off + 5],
            bytes[fde_off + 6],
            bytes[fde_off + 7],
        ]);
        assert_eq!(
            cie_ptr as usize,
            fde_off + 4,
            "FDE CIE_pointer must point back to offset 0 (CIE)"
        );
    }

    #[test]
    fn assemble_has_terminator() {
        let mut cfi = CfiWriter::new();
        cfi.write_cie();
        let bytes = cfi.assemble();
        // Last 4 bytes must be the zero-length terminator record.
        let n = bytes.len();
        assert!(n >= 4);
        assert_eq!(&bytes[n - 4..], &[0, 0, 0, 0], "must end with terminator");
    }

    #[test]
    fn fde_pc_range_matches_func_size() {
        let func_size: u32 = 64;
        let mut cfi = CfiWriter::new();
        cfi.write_cie();
        cfi.write_fde(func_size, &[]);
        let bytes = cfi.assemble();

        // Find the FDE start (right after the CIE).
        let cie_len = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
        let fde_off = 4 + cie_len;
        // FDE layout: [length(4)][CIE_ptr(4)][pc_begin(4)][pc_range(4)][aug_len(ULEB)]...
        let range_off = fde_off + 4 + 4 + 4;
        let range = u32::from_le_bytes([
            bytes[range_off],
            bytes[range_off + 1],
            bytes[range_off + 2],
            bytes[range_off + 3],
        ]);
        assert_eq!(range, func_size, "FDE pc_range must equal function size");
    }

    #[test]
    fn x86_fp_prologue_cfi_sequence() {
        let instrs = x86_fp_prologue_cfi();
        // Should encode: AdvanceLoc(1), DefCfaOffset(16), Offset{6,2}, AdvanceLoc(3), DefCfaRegister(6)
        assert_eq!(instrs.len(), 5);
        assert_eq!(instrs[0], CfiInstr::AdvanceLoc(1));
        assert_eq!(instrs[1], CfiInstr::DefCfaOffset(16));
        assert_eq!(instrs[2], CfiInstr::Offset { reg: 6, factored_offset: 2 });
        assert_eq!(instrs[3], CfiInstr::AdvanceLoc(3));
        assert_eq!(instrs[4], CfiInstr::DefCfaRegister(6));

        // Verify the encoded bytes include def_cfa_offset and def_cfa_register opcodes.
        let encoded: Vec<u8> = instrs.iter().flat_map(|i| i.encode()).collect();
        assert!(encoded.contains(&0x0e), "must contain DW_CFA_def_cfa_offset");
        assert!(encoded.contains(&0x0d), "must contain DW_CFA_def_cfa_register");
    }

    #[test]
    fn multiple_fdes_each_have_correct_cie_pointer() {
        let mut cfi = CfiWriter::new();
        cfi.write_cie();
        cfi.write_fde(10, &[]);
        cfi.write_fde(20, &[]);
        let bytes = cfi.assemble();

        // Walk FDEs and verify each CIE_pointer points back to offset 0.
        let cie_len = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
        let mut pos = 4 + cie_len; // start of first FDE

        for _i in 0..2 {
            assert!(pos + 8 <= bytes.len());
            let fde_body_len =
                u32::from_le_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]])
                    as usize;
            let cie_ptr_field_pos = pos + 4; // offset of the CIE_pointer field in output
            let cie_ptr = u32::from_le_bytes([
                bytes[cie_ptr_field_pos],
                bytes[cie_ptr_field_pos + 1],
                bytes[cie_ptr_field_pos + 2],
                bytes[cie_ptr_field_pos + 3],
            ]);
            // CIE_pointer = distance from cie_ptr_field_pos back to CIE (at 0)
            assert_eq!(
                cie_ptr as usize, cie_ptr_field_pos,
                "FDE at pos {pos} has wrong CIE_pointer"
            );
            pos += 4 + fde_body_len;
        }
    }
}
