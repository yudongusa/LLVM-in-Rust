//! Target-independent code generation: legalization, instruction selection, register allocation, scheduling, and emission.

pub mod emit;
pub mod isel;
pub mod legalize;
pub mod regalloc;
pub mod schedule;

pub use emit::{emit_object, Emitter, ObjectFile, ObjectFormat, Section, Symbol, Reloc, RelocKind};
pub use isel::{IselBackend, MachineFunction, MachineBlock, MInstr, MOpcode, MOperand, PReg, VReg};
