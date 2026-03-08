//! x86_64 calling-convention support (System V AMD64 and Windows x64).

use crate::regs::{R10, R11, R12, R13, R14, R15, R8, R9, RAX, RBP, RBX, RCX, RDI, RDX, RSI};
use llvm_codegen::isel::PReg;

// ── System V AMD64 ABI ─────────────────────────────────────────────────────

/// Integer/pointer argument registers in System V AMD64 order.
pub const SYSV_INT_ARGS: &[PReg] = &[RDI, RSI, RDX, RCX, R8, R9];

/// Integer return register (System V AMD64).
pub const SYSV_INT_RET: PReg = RAX;

/// Caller-saved allocatable GPRs for System V AMD64.
pub const SYSV_ALLOCATABLE: &[PReg] = &[RAX, RCX, RDX, RSI, RDI, R8, R9, R10, R11];

/// Callee-saved GPRs for System V AMD64.
pub const SYSV_CALLEE_SAVED: &[PReg] = &[RBX, RBP, R12, R13, R14, R15];

// ── Windows x64 ABI ───────────────────────────────────────────────────────

/// Integer/pointer argument registers in Windows x64 order.
pub const WIN64_INT_ARGS: &[PReg] = &[RCX, RDX, R8, R9];

/// Integer return register (Windows x64).
pub const WIN64_INT_RET: PReg = RAX;

/// Caller-saved allocatable GPRs for Windows x64.
pub const WIN64_ALLOCATABLE: &[PReg] = &[RAX, RCX, RDX, R8, R9, R10, R11];

/// Callee-saved GPRs for Windows x64.
pub const WIN64_CALLEE_SAVED: &[PReg] = &[RBX, RBP, RSI, RDI, R12, R13, R14, R15];

// ── argument location ──────────────────────────────────────────────────────

/// Where a single argument lands after ABI classification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArgLocation {
    /// Passed in the given integer register.
    Reg(PReg),
    /// Passed on the stack at `offset` bytes above RSP at the call site
    /// (first stack argument is at offset 0).
    Stack(u32),
}

/// Active x86-64 calling convention.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallingConvention {
    SysV,
    Win64,
}

impl CallingConvention {
    /// Infer ABI from target triple (or host OS if triple is absent).
    pub fn from_target_triple(triple: Option<&str>) -> Self {
        if let Some(t) = triple {
            let t = t.to_ascii_lowercase();
            if t.contains("windows") || t.contains("win32") || t.contains("mingw") {
                return Self::Win64;
            }
        }
        if cfg!(target_os = "windows") {
            Self::Win64
        } else {
            Self::SysV
        }
    }

    pub fn classify_int_args(self, n_args: usize) -> Vec<ArgLocation> {
        match self {
            Self::SysV => classify_sysv_args(n_args),
            Self::Win64 => classify_win64_args(n_args),
        }
    }

    pub fn int_ret(self) -> PReg {
        match self {
            Self::SysV => SYSV_INT_RET,
            Self::Win64 => WIN64_INT_RET,
        }
    }

    pub fn allocatable_pregs(self) -> &'static [PReg] {
        match self {
            Self::SysV => SYSV_ALLOCATABLE,
            Self::Win64 => WIN64_ALLOCATABLE,
        }
    }

    pub fn callee_saved_pregs(self) -> &'static [PReg] {
        match self {
            Self::SysV => SYSV_CALLEE_SAVED,
            Self::Win64 => WIN64_CALLEE_SAVED,
        }
    }

    pub fn caller_saved_clobbers(self) -> &'static [PReg] {
        self.allocatable_pregs()
    }

    pub fn shadow_space_bytes(self) -> u32 {
        match self {
            Self::SysV => 0,
            Self::Win64 => 32,
        }
    }
}

/// Classify `n_args` integer/pointer arguments using the System V AMD64 ABI.
///
/// Arguments 0–5 go into registers; arguments 6+ go on the stack.
pub fn classify_sysv_args(n_args: usize) -> Vec<ArgLocation> {
    (0..n_args)
        .map(|i| {
            if i < SYSV_INT_ARGS.len() {
                ArgLocation::Reg(SYSV_INT_ARGS[i])
            } else {
                ArgLocation::Stack(((i - SYSV_INT_ARGS.len()) * 8) as u32)
            }
        })
        .collect()
}

/// Classify `n_args` integer/pointer arguments using the Windows x64 ABI.
///
/// Arguments 0–3 go into registers; arguments 4+ go on the stack.
pub fn classify_win64_args(n_args: usize) -> Vec<ArgLocation> {
    (0..n_args)
        .map(|i| {
            if i < WIN64_INT_ARGS.len() {
                ArgLocation::Reg(WIN64_INT_ARGS[i])
            } else {
                ArgLocation::Stack(((i - WIN64_INT_ARGS.len()) * 8) as u32)
            }
        })
        .collect()
}

// ── tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::regs::{R8, R9, RCX, RDI, RDX, RSI};

    #[test]
    fn sysv_first_six_in_registers() {
        let locs = classify_sysv_args(6);
        assert_eq!(locs[0], ArgLocation::Reg(RDI));
        assert_eq!(locs[1], ArgLocation::Reg(RSI));
        assert_eq!(locs[2], ArgLocation::Reg(RDX));
        assert_eq!(locs[3], ArgLocation::Reg(RCX));
        assert_eq!(locs[4], ArgLocation::Reg(R8));
        assert_eq!(locs[5], ArgLocation::Reg(R9));
    }

    #[test]
    fn sysv_seventh_on_stack() {
        let locs = classify_sysv_args(7);
        assert_eq!(locs[6], ArgLocation::Stack(0));
    }

    #[test]
    fn sysv_eighth_on_stack_offset_8() {
        let locs = classify_sysv_args(8);
        assert_eq!(locs[7], ArgLocation::Stack(8));
    }

    #[test]
    fn win64_first_four_in_registers() {
        let locs = classify_win64_args(4);
        assert_eq!(locs[0], ArgLocation::Reg(RCX));
        assert_eq!(locs[1], ArgLocation::Reg(RDX));
        assert_eq!(locs[2], ArgLocation::Reg(R8));
        assert_eq!(locs[3], ArgLocation::Reg(R9));
    }

    #[test]
    fn win64_fifth_on_stack() {
        let locs = classify_win64_args(5);
        assert_eq!(locs[4], ArgLocation::Stack(0));
    }

    #[test]
    fn zero_args_empty() {
        assert!(classify_sysv_args(0).is_empty());
        assert!(classify_win64_args(0).is_empty());
    }

    #[test]
    fn infer_from_windows_triple() {
        let cc = CallingConvention::from_target_triple(Some("x86_64-pc-windows-msvc"));
        assert_eq!(cc, CallingConvention::Win64);
    }

    #[test]
    fn win64_callee_saved_contains_rsi_rdi() {
        assert!(WIN64_CALLEE_SAVED.contains(&RSI));
        assert!(WIN64_CALLEE_SAVED.contains(&RDI));
        assert!(!WIN64_ALLOCATABLE.contains(&RSI));
        assert!(!WIN64_ALLOCATABLE.contains(&RDI));
    }
}
