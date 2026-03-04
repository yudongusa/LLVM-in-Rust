//! x86_64 target backend: register definitions, instruction set, ABI, IR lowering, and encoding.

pub mod abi;
pub mod encode;
pub mod instructions;
pub mod lower;
pub mod regs;

pub use encode::X86Emitter;
pub use lower::X86Backend;
