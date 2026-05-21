//! DWARF variable location emission.
//!
//! Encodes DW_OP_breg6 (RBP-relative stack) and DW_OP_regN (register)
//! location expressions for DW_TAG_variable DIEs.

/// x86-64 DWARF register numbers (index = our PReg number 0-15).
/// RAX=0, RCX=2, RDX=1, RBX=3, RSI=4, RDI=5, RBP=6, RSP=7, R8-R15=8-15.
pub const X86_DWARF_REG: [u8; 16] = [0, 2, 1, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];

/// SLEB128 encoding (signed LEB128) used in DWARF location expressions.
pub fn encode_sleb128(mut val: i64) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let byte = (val & 0x7f) as u8;
        val >>= 7;
        let more = !((val == 0 && (byte & 0x40) == 0) || (val == -1 && (byte & 0x40) != 0));
        out.push(if more { byte | 0x80 } else { byte });
        if !more {
            break;
        }
    }
    out
}

/// ULEB128 encoding (unsigned LEB128).
pub fn encode_uleb128(mut val: u64) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let byte = (val & 0x7f) as u8;
        val >>= 7;
        out.push(if val != 0 { byte | 0x80 } else { byte });
        if val == 0 {
            break;
        }
    }
    out
}

/// Encode a DW_OP_breg6 (RBP-relative) location expression.
/// DW_OP_breg6 = 0x76, followed by SLEB128 byte offset.
pub fn encode_breg6(rbp_offset: i32) -> Vec<u8> {
    let mut v = vec![0x76u8];
    v.extend(encode_sleb128(rbp_offset as i64));
    v
}

/// Encode a DW_OP_regN (register) location expression.
/// DW_OP_reg0 = 0x50 ... DW_OP_reg15 = 0x5f; DW_OP_regx = 0x90 for higher.
pub fn encode_reg(dwarf_reg: u8) -> Vec<u8> {
    if dwarf_reg <= 15 {
        vec![0x50 + dwarf_reg]
    } else {
        // DW_OP_regx = 0x90 followed by ULEB128
        let mut v = vec![0x90u8];
        v.extend(encode_uleb128(dwarf_reg as u64));
        v
    }
}

/// A variable's location after register allocation.
#[derive(Debug, Clone, PartialEq)]
pub enum VarLocation {
    /// Value lives in this DWARF register number.
    Register(u8),
    /// Value lives at this RBP-relative offset (typically negative).
    Stack(i32),
}

/// Encode a VarLocation to a DWARF location expression byte sequence.
pub fn encode_location(loc: &VarLocation) -> Vec<u8> {
    match loc {
        VarLocation::Register(r) => encode_reg(*r),
        VarLocation::Stack(offset) => encode_breg6(*offset),
    }
}

/// Build a minimal DW_TAG_variable DIE for a named local.
///
/// Returns raw bytes for the DIE contents (not including the abbreviation table).
/// Layout:
///   abbrev_code: ULEB128
///   DW_AT_name:  length(u8) + bytes (DW_FORM_string, null-terminated)
///   DW_AT_type:  4-byte offset (DW_FORM_ref4, placeholder)
///   DW_AT_location: ULEB128 expr_len + expr_bytes (DW_FORM_exprloc)
pub fn build_variable_die(
    abbrev_code: u64,
    name: &str,
    type_die_offset: u32,
    loc: &VarLocation,
) -> Vec<u8> {
    let mut die = Vec::new();
    die.extend(encode_uleb128(abbrev_code));
    // DW_AT_name as DW_FORM_string (null-terminated)
    die.extend_from_slice(name.as_bytes());
    die.push(0u8); // null terminator
    // DW_AT_type as DW_FORM_ref4
    die.extend_from_slice(&type_die_offset.to_le_bytes());
    // DW_AT_location as DW_FORM_exprloc (ULEB128 length + expr)
    let expr = encode_location(loc);
    die.extend(encode_uleb128(expr.len() as u64));
    die.extend(expr);
    die
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sleb128_zero() {
        assert_eq!(encode_sleb128(0), vec![0x00]);
    }

    #[test]
    fn sleb128_positive_small() {
        assert_eq!(encode_sleb128(63), vec![0x3f]);
    }

    #[test]
    fn sleb128_positive_multibyte() {
        // 128 = 0x80: low 7 bits = 0 (not done yet, bit 6 = 0 but val>>7 = 1 ≠ 0),
        // so emit 0x00 | 0x80 = 0x80, then val=1 → emit 0x01.
        assert_eq!(encode_sleb128(128), vec![0x80, 0x01]);
    }

    #[test]
    fn sleb128_negative_one() {
        // -1: byte = 0x7f, val>>7 = -1, bit 6 set → more=false → [0x7f]
        assert_eq!(encode_sleb128(-1), vec![0x7f]);
    }

    #[test]
    fn sleb128_negative_large() {
        // -128: byte = 0x00, val>>7 = -1, bit 6 not set → more=true → emit 0x80
        // next: byte = 0x7f, val>>7 = -1, bit 6 set → more=false → emit 0x7f
        assert_eq!(encode_sleb128(-128), vec![0x80, 0x7f]);
    }

    #[test]
    fn encode_breg6_offset_minus16() {
        // -16 SLEB128: byte = (-16 & 0x7f) = 0x70, val>>7 = -1, bit 6 set → [0x70]
        assert_eq!(encode_breg6(-16), vec![0x76, 0x70]);
    }

    #[test]
    fn encode_breg6_offset_zero() {
        assert_eq!(encode_breg6(0), vec![0x76, 0x00]);
    }

    #[test]
    fn encode_reg_rax() {
        assert_eq!(encode_reg(0), vec![0x50]);
    }

    #[test]
    fn encode_reg_r15() {
        assert_eq!(encode_reg(15), vec![0x5f]);
    }

    #[test]
    fn encode_location_register() {
        assert_eq!(encode_location(&VarLocation::Register(0)), vec![0x50]);
    }

    #[test]
    fn encode_location_stack() {
        // -8 SLEB128: byte = (-8 & 0x7f) = 0x78, val>>7 = -1, bit 6 set → [0x78]
        assert_eq!(encode_location(&VarLocation::Stack(-8)), vec![0x76, 0x78]);
    }

    #[test]
    fn build_variable_die_contains_name() {
        let die = build_variable_die(1, "x", 0, &VarLocation::Register(0));
        // "x\0" should appear in the DIE bytes
        let name_bytes = b"x\0";
        let found = die.windows(name_bytes.len()).any(|w| w == name_bytes);
        assert!(found, "DIE should contain 'x\\0' bytes");
    }

    #[test]
    fn build_variable_die_register_loc() {
        let die = build_variable_die(1, "x", 0, &VarLocation::Register(0));
        // DW_OP_reg0 = 0x50 should appear in the DIE
        assert!(die.contains(&0x50), "DIE should contain DW_OP_reg0 (0x50)");
    }
}
