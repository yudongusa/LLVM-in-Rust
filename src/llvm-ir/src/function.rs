//! Function definition: signature, arguments, basic blocks, and flat instruction pool.

use crate::basic_block::BasicBlock;
use crate::context::{ArgId, BlockId, InstrId, TypeId, ValueRef};
use crate::instruction::Instruction;
use crate::value::{Argument, Linkage};
use std::collections::HashMap;

/// A function definition or declaration.
pub struct Function {
    pub name: String,
    /// Function type (FunctionType TypeId).
    pub ty: TypeId,
    /// Formal arguments.
    pub args: Vec<Argument>,
    /// Basic blocks in program order.
    pub blocks: Vec<BasicBlock>,
    /// Flat instruction pool; `InstrId(i)` indexes `instructions[i]`.
    pub instructions: Vec<Instruction>,
    /// Optional `!dbg !N` attachment for each instruction id.
    pub instr_dbg_locs: HashMap<InstrId, u32>,
    /// Arbitrary metadata attachments per instruction, e.g. `!dbg !12`, `!tbaa !7`.
    pub instr_metadata: HashMap<InstrId, Vec<(String, String)>>,
    /// Maps result name → InstrId.
    pub value_names: HashMap<String, InstrId>,
    /// Maps argument name → ArgId.
    pub arg_names: HashMap<String, ArgId>,
    /// True if this is a declaration (no body).
    pub is_declaration: bool,
    pub linkage: Linkage,
    /// Counter for generating unique names.
    next_name_id: u32,
}

impl Function {
    pub fn new(name: impl Into<String>, ty: TypeId, args: Vec<Argument>, linkage: Linkage) -> Self {
        let mut f = Function {
            name: name.into(),
            ty,
            args: Vec::new(),
            blocks: Vec::new(),
            instructions: Vec::new(),
            instr_dbg_locs: HashMap::new(),
            instr_metadata: HashMap::new(),
            value_names: HashMap::new(),
            arg_names: HashMap::new(),
            is_declaration: false,
            linkage,
            next_name_id: 0,
        };
        for arg in args {
            let idx = ArgId(f.args.len() as u32);
            if !arg.name.is_empty() {
                f.arg_names.insert(arg.name.clone(), idx);
            }
            f.args.push(arg);
        }
        f
    }

    pub fn new_declaration(
        name: impl Into<String>,
        ty: TypeId,
        args: Vec<Argument>,
        linkage: Linkage,
    ) -> Self {
        let mut f = Self::new(name, ty, args, linkage);
        f.is_declaration = true;
        f
    }

    // -----------------------------------------------------------------------
    // Block management
    // -----------------------------------------------------------------------

    /// Add a new basic block and return its `BlockId`.
    pub fn add_block(&mut self, bb: BasicBlock) -> BlockId {
        let id = BlockId(self.blocks.len() as u32);
        self.blocks.push(bb);
        id
    }

    pub fn block(&self, id: BlockId) -> &BasicBlock {
        &self.blocks[id.0 as usize]
    }

    pub fn block_mut(&mut self, id: BlockId) -> &mut BasicBlock {
        &mut self.blocks[id.0 as usize]
    }

    pub fn num_blocks(&self) -> usize {
        self.blocks.len()
    }

    // -----------------------------------------------------------------------
    // Instruction pool
    // -----------------------------------------------------------------------

    /// Allocate an instruction in the flat pool, register its name if any,
    /// and return the `InstrId`.
    pub fn alloc_instr(&mut self, mut instr: Instruction) -> InstrId {
        // Auto-number unnamed value-producing instructions.
        if instr.name.as_deref() == Some("") {
            let name = self.fresh_name();
            instr.name = Some(name);
        }
        let id = InstrId(self.instructions.len() as u32);
        if let Some(ref n) = instr.name {
            if !n.is_empty() {
                self.value_names.insert(n.clone(), id);
            }
        }
        self.instructions.push(instr);
        id
    }

    pub fn instr(&self, id: InstrId) -> &Instruction {
        &self.instructions[id.0 as usize]
    }

    pub fn instr_mut(&mut self, id: InstrId) -> &mut Instruction {
        &mut self.instructions[id.0 as usize]
    }

    pub fn num_instrs(&self) -> usize {
        self.instructions.len()
    }

    pub fn set_instr_dbg_loc(&mut self, id: InstrId, loc_id: u32) {
        self.instr_dbg_locs.insert(id, loc_id);
    }

    pub fn instr_dbg_loc(&self, id: InstrId) -> Option<u32> {
        self.instr_dbg_locs.get(&id).copied()
    }

    pub fn add_instr_metadata(&mut self, id: InstrId, key: impl Into<String>, value: impl Into<String>) {
        self.instr_metadata
            .entry(id)
            .or_default()
            .push((key.into(), value.into()));
    }

    pub fn instr_metadata(&self, id: InstrId) -> Option<&[(String, String)]> {
        self.instr_metadata.get(&id).map(Vec::as_slice)
    }

    // -----------------------------------------------------------------------
    // Arguments
    // -----------------------------------------------------------------------

    pub fn arg(&self, id: ArgId) -> &Argument {
        &self.args[id.0 as usize]
    }

    pub fn num_args(&self) -> usize {
        self.args.len()
    }

    // -----------------------------------------------------------------------
    // Name lookups
    // -----------------------------------------------------------------------

    pub fn lookup_value(&self, name: &str) -> Option<ValueRef> {
        if let Some(&iid) = self.value_names.get(name) {
            return Some(ValueRef::Instruction(iid));
        }
        if let Some(&aid) = self.arg_names.get(name) {
            return Some(ValueRef::Argument(aid));
        }
        None
    }

    pub fn lookup_block(&self, name: &str) -> Option<BlockId> {
        self.blocks
            .iter()
            .enumerate()
            .find(|(_, bb)| bb.name == name)
            .map(|(i, _)| BlockId(i as u32))
    }

    // -----------------------------------------------------------------------
    // Type of SSA values
    // -----------------------------------------------------------------------

    pub fn type_of_value(&self, vref: ValueRef) -> Option<TypeId> {
        match vref {
            ValueRef::Instruction(id) => Some(self.instructions[id.0 as usize].ty),
            ValueRef::Argument(id) => Some(self.args[id.0 as usize].ty),
            ValueRef::Constant(_) | ValueRef::Global(_) => None, // caller must consult Context/Module
        }
    }

    // -----------------------------------------------------------------------
    // Name generation
    // -----------------------------------------------------------------------

    /// Produce a unique name like `"1"`, `"2"`, … for unnamed SSA values.
    pub fn fresh_name(&mut self) -> String {
        let n = self.next_name_id;
        self.next_name_id += 1;
        // Prefix with "v" so the printed name is "%v0", "%v1", etc.  Plain
        // integer names ("%0", "%1") collide with LLVM's implicit slot
        // numbering and are rejected by llvm-as / clang with:
        //   "instruction expected to be numbered '%N' or greater"
        format!("v{n}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::Context;

    #[test]
    fn function_fresh_names() {
        let mut ctx = Context::new();
        let fn_ty = ctx.mk_fn_type(ctx.void_ty, vec![], false);
        let mut f = Function::new("test", fn_ty, vec![], Linkage::External);
        assert_eq!(f.fresh_name(), "v0");
        assert_eq!(f.fresh_name(), "v1");
        assert_eq!(f.fresh_name(), "v2");
    }

    #[test]
    fn function_add_block() {
        let mut ctx = Context::new();
        let fn_ty = ctx.mk_fn_type(ctx.void_ty, vec![], false);
        let mut f = Function::new("test", fn_ty, vec![], Linkage::External);
        let bb = BasicBlock::new("entry");
        let bid = f.add_block(bb);
        assert_eq!(bid, BlockId(0));
        assert_eq!(f.block(bid).name, "entry");
    }
}
