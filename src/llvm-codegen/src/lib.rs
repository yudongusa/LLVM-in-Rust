//! Target-independent code generation: legalization, instruction selection, register allocation, scheduling, and emission.

pub mod emit;
pub mod isel;
pub mod legalize;
pub mod regalloc;
pub mod schedule;

pub use emit::{emit_object, Emitter, ObjectFile, ObjectFormat, Reloc, RelocKind, Section, Symbol};
pub use isel::{IselBackend, MInstr, MOpcode, MOperand, MachineBlock, MachineFunction, PReg, VReg};
