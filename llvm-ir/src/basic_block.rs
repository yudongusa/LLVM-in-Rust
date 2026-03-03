//! Basic block: a sequence of non-terminating instructions ending with a terminator.

use crate::context::InstrId;

/// A basic block within a function.
///
/// The block owns a list of non-terminator `InstrId`s (the body) and an
/// optional terminator `InstrId`. All `InstrId`s index into the owning
/// `Function`'s flat `instructions` pool.
#[derive(Clone, Debug)]
pub struct BasicBlock {
    pub name: String,
    /// Non-terminator instructions, in order.
    pub body: Vec<InstrId>,
    /// The terminator instruction, if present.
    pub terminator: Option<InstrId>,
}

impl BasicBlock {
    pub fn new(name: impl Into<String>) -> Self {
        BasicBlock {
            name: name.into(),
            body: Vec::new(),
            terminator: None,
        }
    }

    /// Append a non-terminator instruction.
    pub fn append_instr(&mut self, id: InstrId) {
        self.body.push(id);
    }

    /// Set the terminator instruction (replaces any existing one).
    pub fn set_terminator(&mut self, id: InstrId) {
        self.terminator = Some(id);
    }

    /// True if the block has a terminator.
    pub fn is_complete(&self) -> bool {
        self.terminator.is_some()
    }

    /// Iterate over all instruction ids in order (body + terminator).
    pub fn instrs(&self) -> impl Iterator<Item = InstrId> + '_ {
        self.body
            .iter()
            .copied()
            .chain(self.terminator.into_iter())
    }

    /// Number of instructions (body + optional terminator).
    pub fn len(&self) -> usize {
        self.body.len() + if self.terminator.is_some() { 1 } else { 0 }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_block_new() {
        let bb = BasicBlock::new("entry");
        assert_eq!(bb.name, "entry");
        assert!(bb.body.is_empty());
        assert!(bb.terminator.is_none());
        assert!(!bb.is_complete());
    }

    #[test]
    fn append_and_terminate() {
        let mut bb = BasicBlock::new("bb0");
        bb.append_instr(InstrId(0));
        bb.append_instr(InstrId(1));
        bb.set_terminator(InstrId(2));
        assert!(bb.is_complete());
        assert_eq!(bb.len(), 3);
        let ids: Vec<_> = bb.instrs().collect();
        assert_eq!(ids, vec![InstrId(0), InstrId(1), InstrId(2)]);
    }
}
