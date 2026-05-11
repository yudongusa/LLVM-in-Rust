//! AArch64 target backend: register definitions, instruction set, ABI, and IR lowering.

pub mod abi;
/// Public API for `encode`.
pub mod encode;
/// Public API for `instructions`.
pub mod instructions;
/// Public API for `lower`.
pub mod lower;
/// Public API for `regs`.
pub mod regs;
