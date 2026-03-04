//! AArch64 calling convention support (AAPCS64).

use crate::regs::{ARG_REGS, RET_REG};
use llvm_codegen::isel::PReg;

// Re-export for convenience.
pub use crate::regs::RET_REG as AAPCS64_INT_RET;

/// Where a single argument lands after ABI classification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArgLocation {
    /// Passed in the given integer register.
    Reg(PReg),
    /// Passed on the stack at `offset` bytes above SP at the call site
    /// (first stack argument is at offset 0, aligned to 8 bytes).
    Stack(i32),
}

/// Classify `n_args` integer/pointer arguments using AAPCS64.
///
/// Arguments 0–7 go into X0–X7; arguments 8+ go on the stack.
pub fn classify_aapcs64_args(n_args: usize) -> Vec<ArgLocation> {
    (0..n_args)
        .map(|i| {
            if i < ARG_REGS.len() {
                ArgLocation::Reg(ARG_REGS[i])
            } else {
                ArgLocation::Stack(((i - ARG_REGS.len()) * 8) as i32)
            }
        })
        .collect()
}

/// The AAPCS64 integer return register (X0).
pub const INT_RET: PReg = RET_REG;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::regs::{X0, X1, X2, X3, X4, X5, X6, X7};

    #[test]
    fn first_eight_in_registers() {
        let locs = classify_aapcs64_args(8);
        assert_eq!(locs[0], ArgLocation::Reg(X0));
        assert_eq!(locs[1], ArgLocation::Reg(X1));
        assert_eq!(locs[2], ArgLocation::Reg(X2));
        assert_eq!(locs[3], ArgLocation::Reg(X3));
        assert_eq!(locs[4], ArgLocation::Reg(X4));
        assert_eq!(locs[5], ArgLocation::Reg(X5));
        assert_eq!(locs[6], ArgLocation::Reg(X6));
        assert_eq!(locs[7], ArgLocation::Reg(X7));
    }

    #[test]
    fn ninth_on_stack_at_offset_0() {
        let locs = classify_aapcs64_args(9);
        assert_eq!(locs[8], ArgLocation::Stack(0));
    }

    #[test]
    fn tenth_on_stack_at_offset_8() {
        let locs = classify_aapcs64_args(10);
        assert_eq!(locs[9], ArgLocation::Stack(8));
    }

    #[test]
    fn zero_args_is_empty() {
        assert!(classify_aapcs64_args(0).is_empty());
    }
}
