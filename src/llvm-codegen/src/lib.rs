//! Target-independent code generation: legalization, instruction selection, register allocation, scheduling, and emission.

pub mod assembler;
/// Public API for `emit`.
pub mod emit;
/// Public API for `isel`.
pub mod isel;
/// Public API for `legalize`.
pub mod legalize;
/// Public API for `regalloc`.
pub mod regalloc;
/// Public API for `regalloc_gc`.
pub mod regalloc_gc;
/// Public API for `schedule`.
pub mod schedule;

/// Public API for `re-export`.
pub use assembler::{
    assemble_bytes, assemble_object, assemble_with_report, AssembledObject, IntegratedAssembler,
    McAssembler, McAssemblyReport,
};
/// Public API for `re-export`.
pub use emit::{emit_object, Emitter, ObjectFile, ObjectFormat, Reloc, RelocKind, Section, Symbol};
/// Public API for `re-export`.
pub use isel::{IselBackend, MInstr, MOpcode, MOperand, MachineBlock, MachineFunction, PReg, VReg};
/// Public API for `re-export`.
pub use regalloc::{
    allocate_registers, apply_allocation, compute_live_intervals, graph_color,
    insert_spill_reloads, linear_scan, RegAllocStrategy,
};
