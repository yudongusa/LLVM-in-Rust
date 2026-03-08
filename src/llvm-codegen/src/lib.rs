//! Target-independent code generation: legalization, instruction selection, register allocation, scheduling, and emission.

pub mod assembler;
pub mod emit;
pub mod isel;
pub mod legalize;
pub mod regalloc;
pub mod regalloc_gc;
pub mod schedule;

pub use assembler::{
    assemble_bytes, assemble_object, assemble_with_report, AssembledObject, IntegratedAssembler,
    McAssembler, McAssemblyReport,
};
pub use emit::{emit_object, Emitter, ObjectFile, ObjectFormat, Reloc, RelocKind, Section, Symbol};
pub use isel::{IselBackend, MInstr, MOpcode, MOperand, MachineBlock, MachineFunction, PReg, VReg};
pub use regalloc::{
    allocate_registers, apply_allocation, compute_live_intervals, graph_color,
    insert_spill_reloads, linear_scan, RegAllocStrategy,
};
