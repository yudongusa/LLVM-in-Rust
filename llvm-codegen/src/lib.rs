//! Target-independent code generation: legalization, instruction selection, register allocation, scheduling, and emission.

pub mod emit;
pub mod isel;
pub mod legalize;
pub mod regalloc;
pub mod schedule;
