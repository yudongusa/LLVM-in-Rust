//! RISC-V target backend (RV64GC): register set, ABI mapping, lowering, and encoding.

pub mod abi;
pub mod encode;
pub mod instructions;
pub mod lower;
pub mod regs;

pub use encode::RiscVEmitter;
pub use lower::RiscVBackend;
