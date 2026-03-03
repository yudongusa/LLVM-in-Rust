//! x86_64 target backend: register definitions, instruction set, ABI, IR lowering, and encoding.

pub mod abi;
pub mod encode;
pub mod instructions;
pub mod lower;
pub mod regs;

pub use lower::X86Backend;
pub use encode::X86Emitter;
