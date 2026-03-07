//! Machine IR types and instruction-selection backend trait.
//!
//! The machine IR (`MachineFunction`, `MInstr`, …) is target-independent.
//! Target backends implement [`IselBackend`] to lower LLVM IR to machine IR.

use llvm_ir::{Context, Function, Module};
use std::collections::HashMap;

// ── indices ────────────────────────────────────────────────────────────────

/// Virtual register (unlimited supply, created during instruction selection).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct VReg(pub u32);

/// Physical register (target-specific numbering).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PReg(pub u8);

/// Opaque machine opcode (each target provides its own constants).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MOpcode(pub u32);

// ── machine operand ────────────────────────────────────────────────────────

/// An operand in a machine instruction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MOperand {
    /// Virtual register (pre-allocation).
    VReg(VReg),
    /// Physical register (post-allocation or ABI-fixed).
    PReg(PReg),
    /// Immediate integer constant.
    Imm(i64),
    /// Branch target: index into `MachineFunction::blocks`.
    Block(usize),
}

// ── machine instruction ────────────────────────────────────────────────────

/// A single machine instruction.
#[derive(Clone, Debug)]
pub struct MInstr {
    /// Target-specific opcode.
    pub opcode: MOpcode,
    /// Output (destination) virtual register, if any.
    pub dst: Option<VReg>,
    /// Input operands (source registers, immediates, branch targets).
    pub operands: Vec<MOperand>,
    /// Physical registers that must hold specific values before this instruction
    /// (e.g. argument registers at a call site).
    pub phys_uses: Vec<PReg>,
    /// Physical registers whose values are destroyed by this instruction
    /// (e.g. caller-saved regs clobbered by a call).
    pub clobbers: Vec<PReg>,
}

impl MInstr {
    pub fn new(opcode: MOpcode) -> Self {
        Self {
            opcode,
            dst: None,
            operands: Vec::new(),
            phys_uses: Vec::new(),
            clobbers: Vec::new(),
        }
    }

    pub fn with_dst(mut self, dst: VReg) -> Self {
        self.dst = Some(dst);
        self
    }
    pub fn with_vreg(mut self, r: VReg) -> Self {
        self.operands.push(MOperand::VReg(r));
        self
    }
    pub fn with_preg(mut self, r: PReg) -> Self {
        self.operands.push(MOperand::PReg(r));
        self
    }
    pub fn with_imm(mut self, imm: i64) -> Self {
        self.operands.push(MOperand::Imm(imm));
        self
    }
    pub fn with_block(mut self, b: usize) -> Self {
        self.operands.push(MOperand::Block(b));
        self
    }
}

// ── machine basic block ────────────────────────────────────────────────────

/// A sequence of machine instructions corresponding to one IR basic block.
#[derive(Clone, Debug, Default)]
pub struct MachineBlock {
    /// Label derived from the IR block name (or function name for entry).
    pub label: String,
    /// Instructions in emission order.
    pub instrs: Vec<MInstr>,
}

// ── machine function ───────────────────────────────────────────────────────

/// Machine-level representation of a function, ready for register allocation
/// and code emission.
#[derive(Clone, Debug)]
pub struct MachineFunction {
    /// Name of the function.
    pub name: String,
    /// Basic blocks in layout order (block 0 is the entry).
    pub blocks: Vec<MachineBlock>,
    /// Counter for allocating fresh virtual registers.
    pub(crate) next_vreg: u32,
    /// Physical registers available for allocation (set by the target).
    pub allocatable_pregs: Vec<PReg>,
    /// Callee-saved physical registers (set by the target).
    pub callee_saved_pregs: Vec<PReg>,
    /// Frame size in bytes (spill slots only; set by insert_spill_reloads).
    pub frame_size: u32,
    /// Map from spilled VReg → frame slot index (0-based; emitter converts to byte offset).
    pub spill_slots: HashMap<VReg, u32>,
    /// Callee-saved physical registers actually used by this function (populated by apply_allocation).
    pub used_callee_saved: Vec<PReg>,
    /// Counter for frame slot allocation.
    next_slot: u32,
    /// Source filename used for debug line tables.
    pub debug_source: Option<String>,
    /// First source line observed from IR `!dbg` metadata.
    pub debug_line_start: Option<u32>,
}

impl MachineFunction {
    pub fn new(name: String) -> Self {
        Self {
            name,
            blocks: Vec::new(),
            next_vreg: 0,
            allocatable_pregs: Vec::new(),
            callee_saved_pregs: Vec::new(),
            frame_size: 0,
            spill_slots: HashMap::new(),
            used_callee_saved: Vec::new(),
            next_slot: 0,
            debug_source: None,
            debug_line_start: None,
        }
    }

    /// Allocate a fresh virtual register.
    pub fn fresh_vreg(&mut self) -> VReg {
        let id = self.next_vreg;
        self.next_vreg += 1;
        VReg(id)
    }

    /// Allocate a fresh frame slot for a spilled VReg and return its index.
    ///
    /// Slot 0 is the first 8-byte slot below the frame pointer.  The emitter
    /// converts a slot index `n` to a byte offset (e.g. x86: `-(n+1)*8` from
    /// RBP; AArch64: `(n+2)*8` above the saved FP/LR pair).
    pub fn alloc_spill_slot(&mut self, vreg: VReg) -> u32 {
        if let Some(&existing) = self.spill_slots.get(&vreg) {
            return existing;
        }
        let slot = self.next_slot;
        self.next_slot += 1;
        self.spill_slots.insert(vreg, slot);
        // Update frame_size: each slot is 8 bytes.
        self.frame_size = self.next_slot * 8;
        slot
    }

    /// Append a new empty machine block and return its index.
    pub fn add_block(&mut self, label: impl Into<String>) -> usize {
        let idx = self.blocks.len();
        self.blocks.push(MachineBlock {
            label: label.into(),
            instrs: Vec::new(),
        });
        idx
    }

    /// Append `instr` to block `block_idx`.
    pub fn push(&mut self, block_idx: usize, instr: MInstr) {
        self.blocks[block_idx].instrs.push(instr);
    }
}

// ── IselBackend trait ──────────────────────────────────────────────────────

/// Implemented by each target to lower LLVM IR functions to machine IR.
pub trait IselBackend {
    /// Lower a single IR function to a [`MachineFunction`].
    fn lower_function(
        &mut self,
        ctx: &Context,
        module: &Module,
        func: &Function,
    ) -> MachineFunction;
}

// ── tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn machine_function_fresh_vreg() {
        let mut mf = MachineFunction::new("f".into());
        let v0 = mf.fresh_vreg();
        let v1 = mf.fresh_vreg();
        assert_eq!(v0, VReg(0));
        assert_eq!(v1, VReg(1));
    }

    #[test]
    fn machine_function_add_block() {
        let mut mf = MachineFunction::new("f".into());
        let b0 = mf.add_block("entry");
        let b1 = mf.add_block("exit");
        assert_eq!(b0, 0);
        assert_eq!(b1, 1);
        assert_eq!(mf.blocks[0].label, "entry");
    }

    #[test]
    fn minstr_builder() {
        let v = VReg(0);
        let p = PReg(1);
        let mi = MInstr::new(MOpcode(42))
            .with_dst(v)
            .with_vreg(v)
            .with_preg(p)
            .with_imm(-7)
            .with_block(3);
        assert_eq!(mi.dst, Some(v));
        assert_eq!(mi.operands.len(), 4);
        assert_eq!(mi.operands[0], MOperand::VReg(v));
        assert_eq!(mi.operands[1], MOperand::PReg(p));
        assert_eq!(mi.operands[2], MOperand::Imm(-7));
        assert_eq!(mi.operands[3], MOperand::Block(3));
    }
}
