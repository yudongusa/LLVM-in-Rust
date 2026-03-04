//! Control-flow graph: predecessor and successor maps over basic blocks.
//!
//! `Cfg::compute` builds the graph from a `Function` by inspecting each
//! block's terminator. Unreachable blocks (no path from entry) are included
//! in the graph but will have no predecessors.

use llvm_ir::{BlockId, Function};

/// Control-flow graph for a single function.
///
/// Block indices map directly to `BlockId(i as u32)`. The entry block is
/// always `BlockId(0)`.
///
/// # Reachability
///
/// The CFG stores edges for **all** blocks, including those unreachable from
/// the entry.  Methods that return block counts or iterate over blocks
/// document whether they include or exclude unreachable blocks:
///
/// | Method | Includes unreachable? |
/// |--------|-----------------------|
/// | `num_blocks()` | yes |
/// | `num_reachable_blocks()` | no |
/// | `rpo()` / `post_order()` | no |
/// | `is_reachable()` | — |
pub struct Cfg {
    num_blocks: usize,
    succs: Vec<Vec<BlockId>>,
    preds: Vec<Vec<BlockId>>,
    /// `reachable[i]` is `true` iff `BlockId(i)` is reachable from entry.
    reachable: Vec<bool>,
    reachable_count: usize,
}

impl Cfg {
    /// Build the CFG for `func` by walking each block's terminator.
    pub fn compute(func: &Function) -> Self {
        let n = func.num_blocks();
        let mut succs = vec![Vec::new(); n];
        let mut preds = vec![Vec::new(); n];

        for (bi, bb) in func.blocks.iter().enumerate() {
            let src = BlockId(bi as u32);
            if let Some(tid) = bb.terminator {
                for dst in func.instr(tid).kind.successors() {
                    succs[bi].push(dst);
                    preds[dst.0 as usize].push(src);
                }
            }
        }

        // DFS from entry to mark reachable blocks.
        let mut reachable = vec![false; n];
        let mut reachable_count = 0;
        if n > 0 {
            let mut stack = vec![0usize];
            while let Some(b) = stack.pop() {
                if reachable[b] {
                    continue;
                }
                reachable[b] = true;
                reachable_count += 1;
                for &succ in &succs[b] {
                    stack.push(succ.0 as usize);
                }
            }
        }

        Cfg {
            num_blocks: n,
            succs,
            preds,
            reachable,
            reachable_count,
        }
    }

    /// The function entry block (always index 0).
    pub fn entry(&self) -> BlockId {
        BlockId(0)
    }

    /// Total number of blocks in the function, **including unreachable blocks**.
    ///
    /// To iterate only reachable blocks use [`rpo`](Self::rpo).
    /// To count only reachable blocks use [`num_reachable_blocks`](Self::num_reachable_blocks).
    pub fn num_blocks(&self) -> usize {
        self.num_blocks
    }

    /// Number of blocks reachable from the entry block.
    ///
    /// This is an O(1) operation. Equivalent to `cfg.rpo().len()` but without
    /// the allocation or DFS traversal cost.
    pub fn num_reachable_blocks(&self) -> usize {
        self.reachable_count
    }

    /// Returns `true` if `bid` is reachable from the entry block.
    ///
    /// # Panics
    ///
    /// Panics if `bid` is not a valid block index for this function (same
    /// behaviour as [`successors`](Self::successors) and
    /// [`predecessors`](Self::predecessors)).
    pub fn is_reachable(&self, bid: BlockId) -> bool {
        self.reachable[bid.0 as usize]
    }

    /// Successor blocks of `bid`.
    pub fn successors(&self, bid: BlockId) -> &[BlockId] {
        &self.succs[bid.0 as usize]
    }

    /// Predecessor blocks of `bid`.
    pub fn predecessors(&self, bid: BlockId) -> &[BlockId] {
        &self.preds[bid.0 as usize]
    }

    /// Blocks in post-order (a block appears after all blocks it can reach).
    ///
    /// **Unreachable blocks are omitted.** Use [`num_blocks`](Self::num_blocks)
    /// if you need a count that includes them.
    pub fn post_order(&self) -> Vec<BlockId> {
        let mut visited = vec![false; self.num_blocks];
        let mut order = Vec::with_capacity(self.num_blocks);
        self.dfs_post(self.entry(), &mut visited, &mut order);
        order
    }

    /// Blocks in reverse post-order (RPO). The entry block is first;
    /// a block always appears before any block it dominates.
    ///
    /// **Unreachable blocks are omitted.** Use [`num_blocks`](Self::num_blocks)
    /// if you need a count that includes them.
    pub fn rpo(&self) -> Vec<BlockId> {
        let mut order = self.post_order();
        order.reverse();
        order
    }

    fn dfs_post(&self, bid: BlockId, visited: &mut Vec<bool>, order: &mut Vec<BlockId>) {
        let idx = bid.0 as usize;
        if visited[idx] {
            return;
        }
        visited[idx] = true;
        for &succ in &self.succs[idx] {
            self.dfs_post(succ, visited, order);
        }
        order.push(bid);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use llvm_ir::{BasicBlock, Context, Function, InstrKind, Instruction, Linkage, ValueRef};

    /// Build a test function with `num_blocks` blocks.
    /// `edges`: (src_idx, vec_of_dst_idx) — at most 2 successors per block.
    /// Blocks not listed get an `unreachable` terminator.
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
                [dst] => InstrKind::Br {
                    dest: BlockId(*dst as u32),
                },
                [t, f] => {
                    let cond = ValueRef::Constant(ctx.const_int(ctx.i1_ty, 0));
                    InstrKind::CondBr {
                        cond,
                        then_dest: BlockId(*t as u32),
                        else_dest: BlockId(*f as u32),
                    }
                }
                _ => panic!("test only supports 0/1/2 successors"),
            };
            let iid = func.alloc_instr(Instruction {
                name: None,
                ty: ctx.void_ty,
                kind,
            });
            func.blocks[src].set_terminator(iid);
        }
        for (i, &needs_term) in has_term.iter().enumerate() {
            if !needs_term {
                let iid = func.alloc_instr(Instruction {
                    name: None,
                    ty: ctx.void_ty,
                    kind: InstrKind::Unreachable,
                });
                func.blocks[i].set_terminator(iid);
            }
        }
        (ctx, func)
    }

    #[test]
    fn cfg_single_block() {
        let (_ctx, func) = build_func(1, &[(0, vec![])]);
        let cfg = Cfg::compute(&func);
        assert_eq!(cfg.num_blocks(), 1);
        assert!(cfg.successors(BlockId(0)).is_empty());
        assert!(cfg.predecessors(BlockId(0)).is_empty());
        assert_eq!(cfg.rpo(), vec![BlockId(0)]);
    }

    #[test]
    fn cfg_linear() {
        let (_ctx, func) = build_func(3, &[(0, vec![1]), (1, vec![2]), (2, vec![])]);
        let cfg = Cfg::compute(&func);
        assert_eq!(cfg.successors(BlockId(0)), &[BlockId(1)]);
        assert_eq!(cfg.successors(BlockId(1)), &[BlockId(2)]);
        assert!(cfg.successors(BlockId(2)).is_empty());
        assert!(cfg.predecessors(BlockId(0)).is_empty());
        assert_eq!(cfg.predecessors(BlockId(2)), &[BlockId(1)]);
        assert_eq!(cfg.rpo(), vec![BlockId(0), BlockId(1), BlockId(2)]);
    }

    #[test]
    fn cfg_diamond() {
        // 0 -> {1, 2} -> 3
        let (_ctx, func) = build_func(
            4,
            &[(0, vec![1, 2]), (1, vec![3]), (2, vec![3]), (3, vec![])],
        );
        let cfg = Cfg::compute(&func);
        assert_eq!(cfg.successors(BlockId(0)).len(), 2);
        assert_eq!(cfg.predecessors(BlockId(3)).len(), 2);
        let rpo = cfg.rpo();
        assert_eq!(rpo[0], BlockId(0));
        assert_eq!(*rpo.last().unwrap(), BlockId(3));
    }

    #[test]
    fn cfg_loop() {
        // 0 -> 1 -> 2 -> {1, 3}  (back-edge 2→1)
        let (_ctx, func) = build_func(
            4,
            &[(0, vec![1]), (1, vec![2]), (2, vec![1, 3]), (3, vec![])],
        );
        let cfg = Cfg::compute(&func);
        assert!(cfg.predecessors(BlockId(1)).contains(&BlockId(2)));
        assert_eq!(cfg.rpo().len(), 4);
    }

    #[test]
    fn cfg_unreachable_block() {
        // 0 -> 1; block 2 is unreachable
        let (_ctx, func) = build_func(3, &[(0, vec![1]), (1, vec![]), (2, vec![])]);
        let cfg = Cfg::compute(&func);
        let rpo = cfg.rpo();
        assert_eq!(rpo.len(), 2);
        assert!(!rpo.contains(&BlockId(2)));
    }

    #[test]
    fn cfg_is_reachable() {
        // 0 -> 1; block 2 is unreachable
        let (_ctx, func) = build_func(3, &[(0, vec![1]), (1, vec![]), (2, vec![])]);
        let cfg = Cfg::compute(&func);
        assert!(cfg.is_reachable(BlockId(0)));
        assert!(cfg.is_reachable(BlockId(1)));
        assert!(!cfg.is_reachable(BlockId(2)));
    }

    #[test]
    fn cfg_num_reachable_blocks() {
        // 0 -> 1; blocks 2 and 3 are unreachable
        let (_ctx, func) = build_func(4, &[(0, vec![1]), (1, vec![]), (2, vec![3]), (3, vec![])]);
        let cfg = Cfg::compute(&func);
        assert_eq!(cfg.num_blocks(), 4);
        assert_eq!(cfg.num_reachable_blocks(), 2);
        // num_reachable_blocks() must equal rpo().len()
        assert_eq!(cfg.num_reachable_blocks(), cfg.rpo().len());
    }

    #[test]
    fn cfg_empty_function_reachable_count() {
        // Zero-block function: num_reachable_blocks() must return 0, not panic.
        let mut ctx = Context::new();
        let fn_ty = ctx.mk_fn_type(ctx.void_ty, vec![], false);
        let func = Function::new("empty", fn_ty, vec![], Linkage::External);
        let cfg = Cfg::compute(&func);
        assert_eq!(cfg.num_blocks(), 0);
        assert_eq!(cfg.num_reachable_blocks(), 0);
    }
}
