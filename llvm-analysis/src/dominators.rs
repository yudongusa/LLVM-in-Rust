//! Dominator tree computed with the iterative dataflow algorithm from
//! Cooper, Harvey & Kennedy — "A Simple, Fast Dominance Algorithm" (2001).
//!
//! Also provides dominance frontier computation (Cytron et al. 1991),
//! required for SSA φ-node placement in `mem2reg`.

use std::collections::HashMap;
use llvm_ir::{BlockId, Function};
use crate::cfg::Cfg;

/// Dominator tree for a single function.
///
/// `idom[i]` is the immediate dominator of block `i`. The entry block's
/// entry is `None` (it has no dominator).
pub struct DomTree {
    /// Immediate dominator of each block. `None` for the entry block and
    /// for blocks unreachable from entry.
    idom: Vec<Option<BlockId>>,
}

impl DomTree {
    /// Compute the dominator tree using the Cooper/Harvey/Kennedy iterative
    /// algorithm on the RPO-numbered CFG.
    pub fn compute(func: &Function, cfg: &Cfg) -> Self {
        let n = func.num_blocks();
        if n == 0 {
            return DomTree { idom: vec![] };
        }

        // Assign each block a position in RPO; unreachable blocks get None.
        let rpo = cfg.rpo();
        let mut rpo_idx: Vec<Option<usize>> = vec![None; n];
        for (i, &bid) in rpo.iter().enumerate() {
            rpo_idx[bid.0 as usize] = Some(i);
        }

        // idom[rpo_pos] = rpo_pos of immediate dominator, or usize::MAX for undefined.
        const UNDEF: usize = usize::MAX;
        let mut idom = vec![UNDEF; rpo.len()];
        idom[0] = 0; // entry dominates itself

        let mut changed = true;
        while changed {
            changed = false;
            // Process blocks in RPO order (skip entry at index 0).
            for i in 1..rpo.len() {
                let bid = rpo[i];
                let preds = cfg.predecessors(bid);

                // Find the first already-processed predecessor.
                let new_idom_opt = preds
                    .iter()
                    .find_map(|&p| rpo_idx[p.0 as usize].filter(|&pi| idom[pi] != UNDEF));

                if let Some(mut new_idom) = new_idom_opt {
                    // Intersect with all other processed predecessors.
                    for &p in preds {
                        let pi = match rpo_idx[p.0 as usize] {
                            Some(pi) if idom[pi] != UNDEF => pi,
                            _ => continue,
                        };
                        if pi != new_idom {
                            new_idom = Self::intersect(pi, new_idom, &idom);
                        }
                    }
                    if idom[i] != new_idom {
                        idom[i] = new_idom;
                        changed = true;
                    }
                }
            }
        }

        // Convert back from RPO positions to BlockIds.
        let mut result = vec![None; n];
        for (i, &bid) in rpo.iter().enumerate() {
            if i == 0 {
                result[bid.0 as usize] = None; // entry has no dominator
            } else if idom[i] != UNDEF {
                result[bid.0 as usize] = Some(rpo[idom[i]]);
            }
        }

        DomTree { idom: result }
    }

    /// Walk up both fingers until they meet — the common dominator.
    fn intersect(mut a: usize, mut b: usize, idom: &[usize]) -> usize {
        while a != b {
            while a > b { a = idom[a]; }
            while b > a { b = idom[b]; }
        }
        a
    }

    /// Immediate dominator of `bid`. Returns `None` for the entry block and
    /// for blocks unreachable from entry.
    pub fn idom(&self, bid: BlockId) -> Option<BlockId> {
        self.idom[bid.0 as usize]
    }

    /// Returns `true` if block `a` dominates block `b`.
    /// Every block dominates itself.
    pub fn dominates(&self, a: BlockId, b: BlockId) -> bool {
        if a == b { return true; }
        self.strictly_dominates(a, b)
    }

    /// Returns `true` if block `a` strictly dominates block `b`
    /// (dominates but is not equal to `b`).
    pub fn strictly_dominates(&self, a: BlockId, b: BlockId) -> bool {
        let mut cur = b;
        loop {
            match self.idom[cur.0 as usize] {
                None => return false,
                Some(p) if p == a => return true,
                Some(p) if p == cur => return false, // entry reached without finding a
                Some(p) => cur = p,
            }
        }
    }

    /// Compute the dominance frontier for every block.
    ///
    /// `DF[b]` = set of blocks y such that b dominates a predecessor of y
    /// but does not strictly dominate y. Used for φ-node placement in SSA.
    pub fn dominance_frontier(&self, cfg: &Cfg) -> HashMap<BlockId, Vec<BlockId>> {
        let mut df: HashMap<BlockId, Vec<BlockId>> = HashMap::new();
        let n = self.idom.len();

        for y_idx in 0..n {
            let y = BlockId(y_idx as u32);
            let preds = cfg.predecessors(y);
            if preds.len() < 2 {
                continue; // only join points have non-empty DF contributions
            }
            for &p in preds {
                let mut runner = p;
                while Some(runner) != self.idom[y_idx] {
                    df.entry(runner).or_default().push(y);
                    match self.idom[runner.0 as usize] {
                        Some(parent) => runner = parent,
                        None => break,
                    }
                }
            }
        }

        // Deduplicate entries (a block can appear multiple times).
        for v in df.values_mut() {
            v.sort_unstable_by_key(|b| b.0);
            v.dedup();
        }
        df
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
    fn domtree_single_block() {
        let (_ctx, func) = build_func(1, &[(0, vec![])]);
        let cfg = Cfg::compute(&func);
        let dom = DomTree::compute(&func, &cfg);
        assert_eq!(dom.idom(BlockId(0)), None);
        assert!(dom.dominates(BlockId(0), BlockId(0)));
    }

    #[test]
    fn domtree_linear() {
        // 0 -> 1 -> 2
        let (_ctx, func) = build_func(3, &[(0, vec![1]), (1, vec![2]), (2, vec![])]);
        let cfg = Cfg::compute(&func);
        let dom = DomTree::compute(&func, &cfg);
        assert_eq!(dom.idom(BlockId(0)), None);
        assert_eq!(dom.idom(BlockId(1)), Some(BlockId(0)));
        assert_eq!(dom.idom(BlockId(2)), Some(BlockId(1)));
        assert!(dom.dominates(BlockId(0), BlockId(2)));
        assert!(!dom.dominates(BlockId(2), BlockId(0)));
        assert!(dom.strictly_dominates(BlockId(0), BlockId(1)));
        assert!(!dom.strictly_dominates(BlockId(1), BlockId(1)));
    }

    #[test]
    fn domtree_diamond() {
        // 0 -> {1,2} -> 3
        let (_ctx, func) = build_func(4, &[
            (0, vec![1, 2]),
            (1, vec![3]),
            (2, vec![3]),
            (3, vec![]),
        ]);
        let cfg = Cfg::compute(&func);
        let dom = DomTree::compute(&func, &cfg);
        // 0 dominates everything; 3's idom is 0 (not 1 or 2).
        assert_eq!(dom.idom(BlockId(3)), Some(BlockId(0)));
        assert!(dom.dominates(BlockId(0), BlockId(3)));
        assert!(!dom.dominates(BlockId(1), BlockId(3)));
        assert!(!dom.dominates(BlockId(2), BlockId(3)));
    }

    #[test]
    fn domtree_loop() {
        // 0 -> 1 -> 2 -> {1, 3}
        let (_ctx, func) = build_func(4, &[
            (0, vec![1]),
            (1, vec![2]),
            (2, vec![1, 3]),
            (3, vec![]),
        ]);
        let cfg = Cfg::compute(&func);
        let dom = DomTree::compute(&func, &cfg);
        assert_eq!(dom.idom(BlockId(1)), Some(BlockId(0)));
        assert_eq!(dom.idom(BlockId(2)), Some(BlockId(1)));
        assert_eq!(dom.idom(BlockId(3)), Some(BlockId(2)));
        // 1 dominates 2 (loop body)
        assert!(dom.dominates(BlockId(1), BlockId(2)));
    }

    #[test]
    fn dominance_frontier_diamond() {
        // 0 -> {1,2} -> 3  — classic phi-placement example
        let (_ctx, func) = build_func(4, &[
            (0, vec![1, 2]),
            (1, vec![3]),
            (2, vec![3]),
            (3, vec![]),
        ]);
        let cfg = Cfg::compute(&func);
        let dom = DomTree::compute(&func, &cfg);
        let df = dom.dominance_frontier(&cfg);
        // DF(1) = DF(2) = {3}, DF(0) = DF(3) = {}
        assert_eq!(df.get(&BlockId(1)).map(|v| v.as_slice()), Some(&[BlockId(3)][..]));
        assert_eq!(df.get(&BlockId(2)).map(|v| v.as_slice()), Some(&[BlockId(3)][..]));
        assert!(df.get(&BlockId(0)).map_or(true, |v| v.is_empty()));
        assert!(df.get(&BlockId(3)).map_or(true, |v| v.is_empty()));
    }

    #[test]
    fn dominance_frontier_loop() {
        // 0 -> 1 -> 2 -> {1, 3}: DF(2) = {1} (back-edge creates frontier)
        let (_ctx, func) = build_func(4, &[
            (0, vec![1]),
            (1, vec![2]),
            (2, vec![1, 3]),
            (3, vec![]),
        ]);
        let cfg = Cfg::compute(&func);
        let dom = DomTree::compute(&func, &cfg);
        let df = dom.dominance_frontier(&cfg);
        assert!(df.get(&BlockId(2)).map_or(false, |v| v.contains(&BlockId(1))));
    }
}

