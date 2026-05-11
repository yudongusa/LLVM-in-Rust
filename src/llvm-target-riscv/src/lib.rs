//! RISC-V target backend (RV64GC): register set, ABI mapping, lowering, and encoding.

pub mod abi;
/// Public API for `encode`.
pub mod encode;
/// Public API for `instructions`.
pub mod instructions;
/// Public API for `lower`.
pub mod lower;
/// Public API for `regs`.
pub mod regs;

/// Public API for `re-export`.
pub use encode::RiscVEmitter;
/// Public API for `re-export`.
pub use lower::RiscVBackend;
