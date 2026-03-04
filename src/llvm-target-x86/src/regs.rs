//! x86_64 physical register definitions.

use llvm_codegen::isel::PReg;

// ── GPR definitions (REX encoding: 0-7 = rax-rdi, 8-15 = r8-r15) ──────────

pub const RAX: PReg = PReg(0);
pub const RCX: PReg = PReg(1);
pub const RDX: PReg = PReg(2);
pub const RBX: PReg = PReg(3);
pub const RSP: PReg = PReg(4);
pub const RBP: PReg = PReg(5);
pub const RSI: PReg = PReg(6);
pub const RDI: PReg = PReg(7);
pub const R8: PReg = PReg(8);
pub const R9: PReg = PReg(9);
pub const R10: PReg = PReg(10);
pub const R11: PReg = PReg(11);
pub const R12: PReg = PReg(12);
pub const R13: PReg = PReg(13);
pub const R14: PReg = PReg(14);
pub const R15: PReg = PReg(15);

/// Caller-saved registers available for allocation (excludes RSP, RBP and
/// the callee-saved set RBX, R12–R15).
pub const ALLOCATABLE: &[PReg] = &[RAX, RCX, RDX, RSI, RDI, R8, R9, R10, R11];

/// Callee-saved registers (must be preserved across calls per System V AMD64).
pub const CALLEE_SAVED: &[PReg] = &[RBX, RBP, R12, R13, R14, R15];

/// Return the 64-bit register name in AT&T / Intel syntax.
pub fn reg_name(r: PReg) -> &'static str {
    match r.0 {
        0 => "rax",
        1 => "rcx",
        2 => "rdx",
        3 => "rbx",
        4 => "rsp",
        5 => "rbp",
        6 => "rsi",
        7 => "rdi",
        8 => "r8",
        9 => "r9",
        10 => "r10",
        11 => "r11",
        12 => "r12",
        13 => "r13",
        14 => "r14",
        15 => "r15",
        _ => "??",
    }
}

/// Whether a register requires the REX prefix (R8–R15).
pub fn is_extended(r: PReg) -> bool {
    r.0 >= 8
}

/// The 3-bit encoding used in the ModRM reg/rm fields (low 3 bits).
pub fn reg_enc(r: PReg) -> u8 {
    r.0 & 7
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocatable_does_not_contain_rsp_rbp() {
        assert!(!ALLOCATABLE.contains(&RSP));
        assert!(!ALLOCATABLE.contains(&RBP));
    }

    #[test]
    fn allocatable_and_callee_saved_disjoint() {
        for r in ALLOCATABLE {
            assert!(!CALLEE_SAVED.contains(r), "{} in both sets", reg_name(*r));
        }
    }

    #[test]
    fn reg_enc_low3_bits() {
        assert_eq!(reg_enc(RAX), 0);
        assert_eq!(reg_enc(R8), 0); // R8 = 8 & 7 = 0
        assert_eq!(reg_enc(R9), 1);
        assert_eq!(reg_enc(RDI), 7);
    }

    #[test]
    fn extended_flag() {
        assert!(!is_extended(RAX));
        assert!(!is_extended(RDI));
        assert!(is_extended(R8));
        assert!(is_extended(R15));
    }
}
