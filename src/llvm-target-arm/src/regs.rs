//! AArch64 physical register definitions.

use llvm_codegen::isel::PReg;

// ── General-purpose 64-bit registers (X0–X30, XZR) ──────────────────────────
//
// The AArch64 encoding uses 5-bit register fields (0–31).
// X31 is context-dependent: XZR (zero register) in most instructions,
// SP (stack pointer) in load/store addressing.

pub const X0: PReg = PReg(0);
pub const X1: PReg = PReg(1);
pub const X2: PReg = PReg(2);
pub const X3: PReg = PReg(3);
pub const X4: PReg = PReg(4);
pub const X5: PReg = PReg(5);
pub const X6: PReg = PReg(6);
pub const X7: PReg = PReg(7);
pub const X8: PReg = PReg(8);
pub const X9: PReg = PReg(9);
pub const X10: PReg = PReg(10);
pub const X11: PReg = PReg(11);
pub const X12: PReg = PReg(12);
pub const X13: PReg = PReg(13);
pub const X14: PReg = PReg(14);
pub const X15: PReg = PReg(15);
pub const X16: PReg = PReg(16); // IP0 (intra-procedure-call scratch)
pub const X17: PReg = PReg(17); // IP1
pub const X18: PReg = PReg(18); // platform register (reserved on some ABIs)
pub const X19: PReg = PReg(19);
pub const X20: PReg = PReg(20);
pub const X21: PReg = PReg(21);
pub const X22: PReg = PReg(22);
pub const X23: PReg = PReg(23);
pub const X24: PReg = PReg(24);
pub const X25: PReg = PReg(25);
pub const X26: PReg = PReg(26);
pub const X27: PReg = PReg(27);
pub const X28: PReg = PReg(28);
pub const X29: PReg = PReg(29); // frame pointer
pub const X30: PReg = PReg(30); // link register
pub const XZR: PReg = PReg(31); // zero register (read: 0, write: discard)

/// Registers available for the register allocator.
/// Caller-saved (X0–X18) minus X18 (platform reserved), plus a subset of
/// callee-saved. We expose X0–X15 as the primary allocation pool.
pub const ALLOCATABLE: &[PReg] = &[
    X0, X1, X2, X3, X4, X5, X6, X7, X8, X9, X10, X11, X12, X13, X14, X15, X19, X20, X21, X22, X23,
    X24, X25, X26, X27, X28,
];

/// Registers that must be preserved across calls (AAPCS64).
pub const CALLEE_SAVED: &[PReg] = &[X19, X20, X21, X22, X23, X24, X25, X26, X27, X28, X29, X30];

/// Integer argument registers (AAPCS64: X0–X7).
pub const ARG_REGS: &[PReg] = &[X0, X1, X2, X3, X4, X5, X6, X7];

/// Integer return register.
pub const RET_REG: PReg = X0;

/// 5-bit register encoding (low 5 bits; always 0–31 for AArch64).
pub fn reg_enc(r: PReg) -> u8 {
    r.0 & 0x1F
}

/// Whether a register is in the "upper" range (X16+).  Some instruction
/// variants or compact encodings have restrictions on these.
pub fn is_extended(r: PReg) -> bool {
    r.0 >= 16
}

/// Human-readable register name.
pub fn reg_name(r: PReg) -> &'static str {
    match r.0 {
        0 => "x0",
        1 => "x1",
        2 => "x2",
        3 => "x3",
        4 => "x4",
        5 => "x5",
        6 => "x6",
        7 => "x7",
        8 => "x8",
        9 => "x9",
        10 => "x10",
        11 => "x11",
        12 => "x12",
        13 => "x13",
        14 => "x14",
        15 => "x15",
        16 => "x16",
        17 => "x17",
        18 => "x18",
        19 => "x19",
        20 => "x20",
        21 => "x21",
        22 => "x22",
        23 => "x23",
        24 => "x24",
        25 => "x25",
        26 => "x26",
        27 => "x27",
        28 => "x28",
        29 => "x29",
        30 => "x30",
        31 => "xzr",
        _ => "??",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocatable_does_not_contain_x29_x30_xzr() {
        assert!(!ALLOCATABLE.contains(&X29));
        assert!(!ALLOCATABLE.contains(&X30));
        assert!(!ALLOCATABLE.contains(&XZR));
    }

    #[test]
    fn arg_regs_are_x0_through_x7() {
        assert_eq!(ARG_REGS, &[X0, X1, X2, X3, X4, X5, X6, X7]);
    }

    #[test]
    fn ret_reg_is_x0() {
        assert_eq!(RET_REG, X0);
    }

    #[test]
    fn reg_enc_low5_bits() {
        assert_eq!(reg_enc(X0), 0);
        assert_eq!(reg_enc(X7), 7);
        assert_eq!(reg_enc(XZR), 31);
        assert_eq!(reg_enc(XZR), 31);
    }

    #[test]
    fn allocatable_and_callee_saved_overlap_correctly() {
        // X19-X28 must appear in BOTH ALLOCATABLE and CALLEE_SAVED.
        for r in &[X19, X20, X21, X22, X23, X24, X25, X26, X27, X28] {
            assert!(
                ALLOCATABLE.contains(r),
                "{} missing from ALLOCATABLE",
                reg_name(*r)
            );
            assert!(
                CALLEE_SAVED.contains(r),
                "{} missing from CALLEE_SAVED",
                reg_name(*r)
            );
        }
    }
}
