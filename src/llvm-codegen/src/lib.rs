//! Target-independent code generation: legalization, instruction selection, register allocation, scheduling, and emission.

pub mod assembler;
pub mod dwarf_vars;
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
pub use emit::{
    emit_globals, emit_object, sizeof_ty, Emitter, ObjectFile, ObjectFormat, Reloc, RelocKind,
    Section, Symbol,
};
/// Public API for `re-export`.
pub use isel::{IselBackend, MInstr, MOpcode, MOperand, MachineBlock, MachineFunction, PReg, RegClass, VReg};
/// Public API for `re-export`.
pub use dwarf_vars::{
    build_variable_die, encode_breg6, encode_location, encode_reg, encode_sleb128,
    encode_uleb128, VarLocation, X86_DWARF_REG,
};
/// Public API for `re-export`.
pub use regalloc::{
    allocate_registers, apply_allocation, compute_live_intervals, graph_color,
    insert_spill_reloads, linear_scan, RegAllocStrategy,
};
/// Public API for `schedule` re-exports.
pub use schedule::{
    apply_schedule, build_dep_dag, compute_critical_paths, list_schedule, x86_latency, DepEdge,
    DepKind,
};
