//! RISC-V integer calling convention helpers (rv64 psABI subset).

use crate::regs::{ARG_REGS, RET_REG};
use llvm_codegen::isel::PReg;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArgLocation {
    Reg(PReg),
    Stack(i32),
}

pub fn classify_rv64_int_args(n_args: usize) -> Vec<ArgLocation> {
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

pub const INT_RET: PReg = RET_REG;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::regs::{X10, X11, X12, X13, X14, X15, X16, X17};

    #[test]
    fn first_eight_go_to_a0_a7() {
        let locs = classify_rv64_int_args(8);
        assert_eq!(locs[0], ArgLocation::Reg(X10));
        assert_eq!(locs[1], ArgLocation::Reg(X11));
        assert_eq!(locs[2], ArgLocation::Reg(X12));
        assert_eq!(locs[3], ArgLocation::Reg(X13));
        assert_eq!(locs[4], ArgLocation::Reg(X14));
        assert_eq!(locs[5], ArgLocation::Reg(X15));
        assert_eq!(locs[6], ArgLocation::Reg(X16));
        assert_eq!(locs[7], ArgLocation::Reg(X17));
    }

    #[test]
    fn ninth_arg_on_stack_0() {
        let locs = classify_rv64_int_args(9);
        assert_eq!(locs[8], ArgLocation::Stack(0));
    }

    #[test]
    fn tenth_arg_on_stack_8() {
        let locs = classify_rv64_int_args(10);
        assert_eq!(locs[9], ArgLocation::Stack(8));
    }

    #[test]
    fn zero_args_empty() {
        assert!(classify_rv64_int_args(0).is_empty());
    }
}
