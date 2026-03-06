//! RISC-V RV64 integer register definitions.

use llvm_codegen::isel::PReg;

pub const X0: PReg = PReg(0); // zero
pub const X1: PReg = PReg(1); // ra
pub const X2: PReg = PReg(2); // sp
pub const X3: PReg = PReg(3); // gp
pub const X4: PReg = PReg(4); // tp
pub const X5: PReg = PReg(5); // t0
pub const X6: PReg = PReg(6); // t1
pub const X7: PReg = PReg(7); // t2
pub const X8: PReg = PReg(8); // s0/fp
pub const X9: PReg = PReg(9); // s1
pub const X10: PReg = PReg(10); // a0
pub const X11: PReg = PReg(11); // a1
pub const X12: PReg = PReg(12); // a2
pub const X13: PReg = PReg(13); // a3
pub const X14: PReg = PReg(14); // a4
pub const X15: PReg = PReg(15); // a5
pub const X16: PReg = PReg(16); // a6
pub const X17: PReg = PReg(17); // a7
pub const X18: PReg = PReg(18); // s2
pub const X19: PReg = PReg(19); // s3
pub const X20: PReg = PReg(20); // s4
pub const X21: PReg = PReg(21); // s5
pub const X22: PReg = PReg(22); // s6
pub const X23: PReg = PReg(23); // s7
pub const X24: PReg = PReg(24); // s8
pub const X25: PReg = PReg(25); // s9
pub const X26: PReg = PReg(26); // s10
pub const X27: PReg = PReg(27); // s11
pub const X28: PReg = PReg(28); // t3
pub const X29: PReg = PReg(29); // t4
pub const X30: PReg = PReg(30); // t5
pub const X31: PReg = PReg(31); // t6

pub const ARG_REGS: &[PReg] = &[X10, X11, X12, X13, X14, X15, X16, X17];
pub const RET_REG: PReg = X10;

pub const CALLEE_SAVED: &[PReg] = &[X9, X18, X19, X20, X21, X22, X23, X24, X25, X26, X27];

pub const ALLOCATABLE: &[PReg] = &[
    X10, X11, X12, X13, X14, X15, X16, X17, // a0-a7
    X5, X6, X7, X28, X29, X30, X31, // t0-t6
    X9, X18, X19, X20, X21, X22, X23, X24, X25, X26, X27, // s1,s2-s11
];

pub fn reg_enc(r: PReg) -> u8 {
    r.0 & 0x1F
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arg_regs_are_a0_to_a7() {
        assert_eq!(ARG_REGS, &[X10, X11, X12, X13, X14, X15, X16, X17]);
    }

    #[test]
    fn ret_reg_is_a0() {
        assert_eq!(RET_REG, X10);
    }

    #[test]
    fn allocatable_excludes_zero_sp_gp_tp_ra_fp() {
        for r in [X0, X1, X2, X3, X4, X8] {
            assert!(!ALLOCATABLE.contains(&r));
        }
    }

    #[test]
    fn callee_saved_subset_in_allocatable() {
        for r in CALLEE_SAVED {
            assert!(ALLOCATABLE.contains(r));
        }
    }
}
