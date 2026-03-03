//! Natural loop detection built on the dominator tree.
//!
//! Algorithm:
//! 1. Walk the CFG with DFS to find back-edges — edges (n → h) where h
//!    dominates n.
//! 2. For each back-edge, collect the natural loop body via reverse-CFG BFS
//!    from the tail up to (and including) the header.
//! 3. Assign loop nesting: a loop is a child of the smallest loop whose body
//!    contains the child's header.

use std::collections::{HashMap, HashSet, VecDeque};
use llvm_ir::{BlockId, Function};
use crate::cfg::Cfg;
use crate::dominators::DomTree;

/// A single natural loop.
#[derive(Debug)]
pub struct Loop {
    /// The loop header (target of the back-edge).
    pub header: BlockId,
    /// All blocks in the loop body, including the header. Sorted by BlockId.
    pub body: Vec<BlockId>,
    /// Index of the immediately enclosing loop in `LoopInfo::loops`, if any.
    pub parent: Option<usize>,
}

/// Loop nesting information for a single function.
pub struct LoopInfo {
    loops: Vec<Loop>,
    /// Maps each block to the index of its innermost containing loop.
    block_loop: HashMap<BlockId, usize>,
}

impl LoopInfo {
    /// Detect all natural loops in `func`.
    pub fn compute(func: &Function, cfg: &Cfg, dom: &DomTree) -> Self {
        if func.num_blocks() == 0 {
            return LoopInfo { loops: vec![], block_loop: HashMap::new() };
        }

        // Find back-edges and build one loop per back-edge.
        let back_edges = Self::find_back_edges(cfg, dom);
        let mut loops: Vec<Loop> = back_edges
            .into_iter()
            .map(|(tail, header)| Loop {
                header,
                body: Self::collect_loop_body(tail, header, cfg),
                parent: None,
            })
            .collect();

        // Sort largest-body first so parent search is straightforward.
        loops.sort_by(|a, b| b.body.len().cmp(&a.body.len()));

        // Assign parent: the innermost (smallest body) loop that contains
        // this loop's header, excluding itself.
        for i in 0..loops.len() {
            let header = loops[i].header;
            loops[i].parent = loops
                .iter()
                .enumerate()
                .filter(|(j, l)| *j != i && l.body.contains(&header))
                .min_by_key(|(_, l)| l.body.len())
                .map(|(j, _)| j);
        }

        // Build block → innermost loop map.
        let mut block_loop: HashMap<BlockId, usize> = HashMap::new();
        for (i, lp) in loops.iter().enumerate() {
            for &b in &lp.body {
                let entry = block_loop.entry(b).or_insert(i);
                if loops[i].body.len() < loops[*entry].body.len() {
                    *entry = i;
                }
            }
        }

        LoopInfo { loops, block_loop }
    }

    /// Find all back-edges (tail, header) using iterative DFS on the CFG.
    /// An edge (n → h) is a back-edge when h dominates n.
    fn find_back_edges(cfg: &Cfg, dom: &DomTree) -> Vec<(BlockId, BlockId)> {
        let mut back_edges = Vec::new();
        let mut visited = HashSet::new();
        let mut stack = vec![cfg.entry()];
        while let Some(b) = stack.pop() {
            if !visited.insert(b) { continue; }
            for &succ in cfg.successors(b) {
                if dom.dominates(succ, b) {
                    back_edges.push((b, succ));
                } else {
                    stack.push(succ);
                }
            }
        }
        back_edges
    }

    /// Collect all blocks in the natural loop for back-edge (tail → header)
    /// using reverse-CFG BFS starting at `tail`, stopping at `header`.
    fn collect_loop_body(tail: BlockId, header: BlockId, cfg: &Cfg) -> Vec<BlockId> {
        let mut body: HashSet<BlockId> = HashSet::new();
        body.insert(header);
        let mut queue = VecDeque::new();
        if body.insert(tail) {
            queue.push_back(tail);
        }
        while let Some(b) = queue.pop_front() {
            for &pred in cfg.predecessors(b) {
                if body.insert(pred) {
                    queue.push_back(pred);
                }
            }
        }
        let mut v: Vec<BlockId> = body.into_iter().collect();
        v.sort_unstable_by_key(|b| b.0);
        v
    }

    /// All detected loops.
    pub fn loops(&self) -> &[Loop] {
        &self.loops
    }

    /// Index of the innermost loop containing `bid`, or `None`.
    pub fn loop_of(&self, bid: BlockId) -> Option<usize> {
        self.block_loop.get(&bid).copied()
    }

    /// `true` if `bid` is the header of any detected loop.
    pub fn is_loop_header(&self, bid: BlockId) -> bool {
        self.loops.iter().any(|l| l.header == bid)
    }

    /// Nesting depth of `bid` (0 = not in a loop, 1 = outermost loop, …).
    pub fn depth(&self, bid: BlockId) -> usize {
        let mut idx = match self.block_loop.get(&bid) {
            None => return 0,
            Some(&i) => i,
        };
        let mut d = 1;
        while let Some(p) = self.loops[idx].parent {
            idx = p;
            d += 1;
        }
        d
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use llvm_ir::{Context, Function, BasicBlock, Instruction, InstrKind, Linkage, ValueRef};

    fn build_func(num_blocks: usize, edges: &[(usize, Vec<usize>)]) -> (Context, Function) {
        let mut ctx = Context::new();
        let fn_ty = ctx.mk_fn_type(ctx.void_ty, vec![], false);
        let mut func = Function::new("test", fn_ty, vec![], Linkage::External);
        for i in 0..num_blocks {
            func.add_block(BasicBlock::new(format!("b{}", i)));
        }
        let mut has_term = vec![false; num_blocks];
        for &(src, ref dsts) in edges {
            has_term[src] = true;
            let kind = match dsts.as_slice() {
                [] => InstrKind::Unreachable,
                [dst] => InstrKind::Br { dest: BlockId(*dst as u32) },
                [t, f] => {
                    let cond = ValueRef::Constant(ctx.const_int(ctx.i1_ty, 0));
                    InstrKind::CondBr { cond, then_dest: BlockId(*t as u32), else_dest: BlockId(*f as u32) }
                }
                _ => panic!("max 2 successors"),
            };
            let iid = func.alloc_instr(Instruction { name: None, ty: ctx.void_ty, kind });
            func.blocks[src].set_terminator(iid);
        }
        for i in 0..num_blocks {
            if !has_term[i] {
                let iid = func.alloc_instr(Instruction { name: None, ty: ctx.void_ty, kind: InstrKind::Unreachable });
                func.blocks[i].set_terminator(iid);
            }
        }
        (ctx, func)
    }

    #[test]
    fn no_loops() {
        let (_ctx, func) = build_func(3, &[(0, vec![1]), (1, vec![2]), (2, vec![])]);
        let cfg = Cfg::compute(&func);
        let dom = DomTree::compute(&func, &cfg);
        let li = LoopInfo::compute(&func, &cfg, &dom);
        assert!(li.loops().is_empty());
        assert_eq!(li.depth(BlockId(0)), 0);
        assert_eq!(li.depth(BlockId(1)), 0);
        assert_eq!(li.depth(BlockId(2)), 0);
    }

    #[test]
    fn simple_loop() {
        // 0 -> 1 -> 2 -> {1(back), 3}
        let (_ctx, func) = build_func(4, &[
            (0, vec![1]),
            (1, vec![2]),
            (2, vec![1, 3]),
            (3, vec![]),
        ]);
        let cfg = Cfg::compute(&func);
        let dom = DomTree::compute(&func, &cfg);
        let li = LoopInfo::compute(&func, &cfg, &dom);

        assert_eq!(li.loops().len(), 1);
        assert_eq!(li.loops()[0].header, BlockId(1));
        assert!(li.loops()[0].body.contains(&BlockId(1)));
        assert!(li.loops()[0].body.contains(&BlockId(2)));
        assert!(!li.loops()[0].body.contains(&BlockId(0)));
        assert!(!li.loops()[0].body.contains(&BlockId(3)));

        assert!(li.is_loop_header(BlockId(1)));
        assert!(!li.is_loop_header(BlockId(0)));
        assert_eq!(li.depth(BlockId(0)), 0);
        assert_eq!(li.depth(BlockId(1)), 1);
        assert_eq!(li.depth(BlockId(2)), 1);
        assert_eq!(li.depth(BlockId(3)), 0);
    }

    #[test]
    fn nested_loops() {
        // Outer loop header=1: body={1,2,3}
        // Inner loop header=2: body={2,3}
        // CFG: 0->1, 1->{2,4}, 2->3, 3->{2(inner back),1(outer back)}, 4 exit
        let (_ctx, func) = build_func(5, &[
            (0, vec![1]),
            (1, vec![2, 4]),
            (2, vec![3]),
            (3, vec![2, 1]),
            (4, vec![]),
        ]);
        let cfg = Cfg::compute(&func);
        let dom = DomTree::compute(&func, &cfg);
        let li = LoopInfo::compute(&func, &cfg, &dom);

        assert_eq!(li.loops().len(), 2);
        let headers: Vec<BlockId> = li.loops().iter().map(|l| l.header).collect();
        assert!(headers.contains(&BlockId(1)));
        assert!(headers.contains(&BlockId(2)));

        // Outer loop contains blocks 1,2,3; inner contains 2,3.
        assert_eq!(li.depth(BlockId(1)), 1); // only in outer
        assert_eq!(li.depth(BlockId(2)), 2); // in both
        assert_eq!(li.depth(BlockId(3)), 2); // in both
        assert_eq!(li.depth(BlockId(0)), 0);
        assert_eq!(li.depth(BlockId(4)), 0);
    }
}
