//! x86_64 target backend: register definitions, instruction set, ABI, IR lowering, and encoding.

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
pub use encode::X86Emitter;
/// Public API for `re-export`.
pub use lower::{TargetFeatures, X86Backend};
