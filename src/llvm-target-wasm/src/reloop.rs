//! Relooper: converts an IR CFG to WebAssembly structured control flow.
//!
//! The algorithm works in three phases:
//! 1. Compute RPO order from the entry block via DFS.
//! 2. Detect back-edges (edges A→B where B's RPO index < A's RPO index).
//! 3. Recursively build the `ControlNode` tree by grouping loop bodies and
//!    if/else branches.

use llvm_ir::{BlockId, Function};
use std::collections::{HashMap, HashSet};

// ── public types ─────────────────────────────────────────────────────────────

/// A node in the structured control-flow tree produced by the Relooper.
#[derive(Debug, Clone)]
pub enum ControlNode {
    /// A single basic block followed by an optional continuation.
    Simple {
        id: BlockId,
        next: Option<Box<ControlNode>>,
    },
    /// A loop: `header` is the loop-entry block; `body` is the structured body.
    Loop {
        header: BlockId,
        body: Box<ControlNode>,
        next: Option<Box<ControlNode>>,
    },
    /// An if/else split at `cond_block`.
    Branch {
        cond_block: BlockId,
        then_node: Box<ControlNode>,
        else_node: Option<Box<ControlNode>>,
        next: Option<Box<ControlNode>>,
    },
}

// ── public API ────────────────────────────────────────────────────────────────

/// Build a structured control-flow tree from the CFG of `func`.
///
/// The algorithm uses RPO ordering and RPO-index-based back-edge detection.
/// An edge A→B is a back-edge iff B's RPO index is strictly less than A's.
///
/// Returns a `ControlNode::Simple` wrapping the entry block when the function
/// has no blocks, or a full tree otherwise.
pub fn build_control_tree(func: &Function) -> ControlNode {
    if func.blocks.is_empty() {
        // Degenerate: return a sentinel node that callers can ignore.
        return ControlNode::Simple { id: BlockId(0), next: None };
    }

    // Step 1: compute RPO order (entry block first).
    let rpo = compute_rpo(func);

    // Step 2: build RPO-index map.
    let rpo_index: HashMap<BlockId, usize> = rpo
        .iter()
        .enumerate()
        .map(|(i, &b)| (b, i))
        .collect();

    // Step 3: detect which blocks are loop headers (back-edge targets).
    let loop_headers: HashSet<BlockId> = detect_loop_headers(func, &rpo_index);

    // Step 4: recursively build the tree over the RPO block sequence.
    build_tree(func, &rpo, 0, &rpo_index, &loop_headers)
        .unwrap_or(ControlNode::Simple { id: BlockId(0), next: None })
}

/// Returns `true` if a switch with the given case values should use a `br_table`
/// instruction.
///
/// The heuristic: use `br_table` when there are ≥3 cases and the value range
/// is dense (max − min < 2 × count).
pub fn should_use_br_table(cases: &[i64]) -> bool {
    if cases.len() < 3 {
        return false;
    }
    let min = cases.iter().copied().min().unwrap();
    let max = cases.iter().copied().max().unwrap();
    (max - min) < (cases.len() as i64 * 2)
}

// ── internal helpers ──────────────────────────────────────────────────────────

/// Compute RPO via iterative post-order DFS, then reverse.
fn compute_rpo(func: &Function) -> Vec<BlockId> {
    if func.blocks.is_empty() {
        return vec![];
    }
    let n = func.blocks.len();
    let mut visited = vec![false; n];
    let mut post_order: Vec<BlockId> = Vec::with_capacity(n);

    // Iterative DFS using an explicit stack.
    // Each entry is (block_id, successor_cursor).
    let mut stack: Vec<(BlockId, usize)> = vec![(BlockId(0), 0)];
    visited[0] = true;

    while let Some((bid, cursor)) = stack.last_mut() {
        let bid = *bid;
        let succs = block_successors(func, bid);
        if *cursor < succs.len() {
            let succ = succs[*cursor];
            *cursor += 1;
            let idx = succ.0 as usize;
            if idx < n && !visited[idx] {
                visited[idx] = true;
                stack.push((succ, 0));
            }
        } else {
            // All successors processed — append to post-order and pop.
            post_order.push(bid);
            stack.pop();
        }
    }

    post_order.reverse(); // post-order → reverse post-order
    post_order
}

/// Return the successor blocks of `bid` by inspecting the terminator.
fn block_successors(func: &Function, bid: BlockId) -> Vec<BlockId> {
    let bb = &func.blocks[bid.0 as usize];
    if let Some(tid) = bb.terminator {
        func.instr(tid).successors()
    } else {
        vec![]
    }
}

/// Detect loop headers: a block H is a loop header iff there exists an edge
/// A→H where `rpo_index[A] >= rpo_index[H]` (i.e. H appears earlier in RPO).
fn detect_loop_headers(
    func: &Function,
    rpo_index: &HashMap<BlockId, usize>,
) -> HashSet<BlockId> {
    let mut headers = HashSet::new();
    for (i, bb) in func.blocks.iter().enumerate() {
        let src = BlockId(i as u32);
        let Some(&src_rpo) = rpo_index.get(&src) else { continue };
        if let Some(tid) = bb.terminator {
            for dst in func.instr(tid).successors() {
                if let Some(&dst_rpo) = rpo_index.get(&dst) {
                    if dst_rpo <= src_rpo {
                        // Back-edge: dst is a loop header.
                        headers.insert(dst);
                    }
                }
            }
        }
    }
    headers
}

/// Collect all blocks that belong to the loop body rooted at `header`.
///
/// A block belongs to the loop body if it can reach `header` via non-back
/// edges (i.e. edges that do not go backwards in RPO order).  We compute
/// this by walking backwards from `header` in the RPO slice up to and
/// including all predecessors whose RPO index is >= `header_rpo`.
fn collect_loop_body(
    func: &Function,
    rpo: &[BlockId],
    header_rpo: usize,
    rpo_index: &HashMap<BlockId, usize>,
) -> Vec<BlockId> {
    // The loop body consists of all blocks in RPO[header_rpo ..] that can
    // reach the header before leaving the loop.  For our simplified algorithm
    // we include all blocks between the header (inclusive) and the first block
    // that is not reachable from the header without going through a back-edge.
    //
    // Simple approach: scan RPO from header_rpo forward; a block belongs to
    // the loop body if any of its predecessors (reachable so far) point back
    // to the header.  For the common case this is just the contiguous slice
    // up to the first block with no predecessor in the slice.
    //
    // We use a worklist: start with the header; add any successor whose
    // RPO index is > header_rpo (forward edges stay inside the loop until
    // they escape).  A successor that is the header itself is the back-edge
    // and marks the loop tail.
    let header = rpo[header_rpo];
    let mut body: HashSet<BlockId> = HashSet::new();
    body.insert(header);

    let mut worklist: Vec<BlockId> = vec![header];
    while let Some(cur) = worklist.pop() {
        let cur_rpo = rpo_index[&cur];
        for succ in block_successors(func, cur) {
            if succ == header {
                // Back-edge — the tail is already a body member.
                continue;
            }
            if let Some(&succ_rpo) = rpo_index.get(&succ) {
                if succ_rpo > cur_rpo && body.insert(succ) {
                    worklist.push(succ);
                }
            }
        }
    }

    // Remove any block that has a successor outside the body AND that
    // successor is closer to the header in RPO than itself — that's an exit.
    // We keep it simple: the body is all blocks we collected.
    let mut v: Vec<BlockId> = body.into_iter().collect();
    v.sort_unstable_by_key(|b| rpo_index[b]);
    v
}

/// Recursively build the `ControlNode` tree starting at `rpo[start]`.
///
/// Returns `None` when `start >= rpo.len()` (past the end).
fn build_tree(
    func: &Function,
    rpo: &[BlockId],
    start: usize,
    rpo_index: &HashMap<BlockId, usize>,
    loop_headers: &HashSet<BlockId>,
) -> Option<ControlNode> {
    if start >= rpo.len() {
        return None;
    }

    let bid = rpo[start];

    // ── case 1: loop header ───────────────────────────────────────────────
    if loop_headers.contains(&bid) {
        // Gather the loop body blocks (all contiguous RPO blocks that form the
        // loop body).
        let body_blocks = collect_loop_body(func, rpo, start, rpo_index);
        let body_len = body_blocks.len();

        // The first block after the loop body is the "next" continuation.
        let after_loop = start + body_len;

        // Build the body sub-tree (starting at the loop header, but treating
        // the back-edges as exits, not recursive loops, to avoid infinite
        // recursion).  We pass a modified loop_headers set that does NOT mark
        // this header as a loop header for the inner call.
        let mut inner_headers = loop_headers.clone();
        inner_headers.remove(&bid);

        // The body sub-tree covers rpo[start .. start+body_len].
        // We slice the RPO to the body, then build over it.
        let body_rpo: Vec<BlockId> = body_blocks.clone();
        let body_node = build_tree_over_slice(func, &body_rpo, 0, rpo_index, &inner_headers);

        let next = build_tree(func, rpo, after_loop, rpo_index, loop_headers);

        return Some(ControlNode::Loop {
            header: bid,
            body: Box::new(body_node.unwrap_or(ControlNode::Simple { id: bid, next: None })),
            next: next.map(Box::new),
        });
    }

    // ── case 2: conditional branch (if/else) ──────────────────────────────
    let succs = block_successors(func, bid);
    if succs.len() == 2 {
        let then_dest = succs[0];
        let else_dest = succs[1];

        // Find where the two branches meet (the "join" point).
        // Simple heuristic: the join is the first block in RPO after `start`
        // that appears in both branch sub-sequences.  For our simplified
        // implementation we emit each branch until we reach the other's
        // first block or the end.
        let then_rpo = rpo_index.get(&then_dest).copied();
        let else_rpo = rpo_index.get(&else_dest).copied();

        // Both branches must be forward edges (not back-edges) for this to
        // be treated as an if/else — back-edges are handled by the loop case.
        let then_is_forward = then_rpo.is_some_and(|r| r > start);
        let else_is_forward = else_rpo.is_some_and(|r| r > start);

        if then_is_forward && else_is_forward {
            let then_start = then_rpo.unwrap();
            let else_start = else_rpo.unwrap();

            // The join point is the min of the two starts when both are
            // forward; everything from there onwards is the continuation.
            // Branches cover [branch_start .. join).
            let join = then_start.min(else_start);

            // "Then" branch: the higher RPO index (further from current).
            // "Else" branch: the lower RPO index (closer to current).
            // Convention: then_dest is the first successor, else_dest is the
            // second.  We build each branch over its slice.
            let then_node = if then_start < else_start {
                // then comes before else; else_start is join
                build_tree_over_slice(
                    func,
                    &rpo[then_start..else_start],
                    0,
                    rpo_index,
                    loop_headers,
                )
            } else if then_start > else_start {
                // else comes before then; then_start is join
                // then branch is empty from else's perspective — emit a simple node
                build_tree_over_slice(
                    func,
                    &rpo[then_start..],
                    0,
                    rpo_index,
                    loop_headers,
                )
            } else {
                // Both branch to the same target — no then/else distinction.
                None
            };

            let else_node = if else_start < then_start {
                build_tree_over_slice(
                    func,
                    &rpo[else_start..then_start],
                    0,
                    rpo_index,
                    loop_headers,
                )
            } else if else_start > then_start {
                build_tree_over_slice(
                    func,
                    &rpo[else_start..],
                    0,
                    rpo_index,
                    loop_headers,
                )
            } else {
                None
            };

            let next = build_tree(func, rpo, join, rpo_index, loop_headers);

            return Some(ControlNode::Branch {
                cond_block: bid,
                then_node: Box::new(
                    then_node.unwrap_or(ControlNode::Simple { id: then_dest, next: None }),
                ),
                else_node: else_node.map(Box::new),
                next: next.map(Box::new),
            });
        }
    }

    // ── case 3: simple block ─────────────────────────────────────────────
    let next = build_tree(func, rpo, start + 1, rpo_index, loop_headers);
    Some(ControlNode::Simple {
        id: bid,
        next: next.map(Box::new),
    })
}

/// Build a tree over an explicit `slice` of BlockIds (not the global RPO array).
///
/// This is used for loop bodies and branch sub-sequences where we want to
/// restrict the search to a subset of the RPO.
fn build_tree_over_slice(
    func: &Function,
    slice: &[BlockId],
    start: usize,
    rpo_index: &HashMap<BlockId, usize>,
    loop_headers: &HashSet<BlockId>,
) -> Option<ControlNode> {
    if start >= slice.len() {
        return None;
    }

    let bid = slice[start];

    // ── loop header within the slice? ─────────────────────────────────────
    if loop_headers.contains(&bid) {
        // Find how many blocks in this slice belong to this loop body.
        let body_blocks = collect_loop_body(func, slice, start, rpo_index);
        let body_len = body_blocks.len();
        let after_loop = start + body_len;

        let mut inner_headers = loop_headers.clone();
        inner_headers.remove(&bid);

        let body_node =
            build_tree_over_slice(func, &body_blocks, 0, rpo_index, &inner_headers);

        let next = build_tree_over_slice(func, slice, after_loop, rpo_index, loop_headers);

        return Some(ControlNode::Loop {
            header: bid,
            body: Box::new(body_node.unwrap_or(ControlNode::Simple { id: bid, next: None })),
            next: next.map(Box::new),
        });
    }

    // ── conditional branch within the slice? ──────────────────────────────
    let succs = block_successors(func, bid);
    if succs.len() == 2 {
        let then_dest = succs[0];
        let else_dest = succs[1];

        // Map targets to positions within this slice.
        let then_pos = slice.iter().position(|&b| b == then_dest);
        let else_pos = slice.iter().position(|&b| b == else_dest);

        if let (Some(tp), Some(ep)) = (then_pos, else_pos) {
            if tp > start && ep > start {
                let join = tp.min(ep);

                let then_node = if tp < ep {
                    build_tree_over_slice(func, &slice[tp..ep], 0, rpo_index, loop_headers)
                } else if tp > ep {
                    build_tree_over_slice(func, &slice[tp..], 0, rpo_index, loop_headers)
                } else {
                    None
                };

                let else_node = if ep < tp {
                    build_tree_over_slice(func, &slice[ep..tp], 0, rpo_index, loop_headers)
                } else if ep > tp {
                    build_tree_over_slice(func, &slice[ep..], 0, rpo_index, loop_headers)
                } else {
                    None
                };

                let next = build_tree_over_slice(func, slice, join, rpo_index, loop_headers);

                return Some(ControlNode::Branch {
                    cond_block: bid,
                    then_node: Box::new(
                        then_node
                            .unwrap_or(ControlNode::Simple { id: then_dest, next: None }),
                    ),
                    else_node: else_node.map(Box::new),
                    next: next.map(Box::new),
                });
            }
        }
    }

    // ── simple block ─────────────────────────────────────────────────────
    let next = build_tree_over_slice(func, slice, start + 1, rpo_index, loop_headers);
    Some(ControlNode::Simple {
        id: bid,
        next: next.map(Box::new),
    })
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use llvm_ir::{BasicBlock, Context, Function, InstrKind, Instruction, Linkage, ValueRef};

    // ── helper: build a Function from a list of edges ─────────────────────

    /// Build a test function with `num_blocks` basic blocks.
    ///
    /// `edges`: `(src, dsts)` — sets the terminator of block `src`.
    /// - 0 successors → `Unreachable`
    /// - 1 successor  → `Br`
    /// - 2 successors → `CondBr` (condition = constant `i1 false`)
    fn build_func(num_blocks: usize, edges: &[(usize, &[usize])]) -> (Context, Function) {
        let mut ctx = Context::new();
        let fn_ty = ctx.mk_fn_type(ctx.void_ty, vec![], false);
        let mut func = Function::new("test", fn_ty, vec![], Linkage::External);

        for i in 0..num_blocks {
            func.add_block(BasicBlock::new(format!("b{i}")));
        }

        let mut has_term = vec![false; num_blocks];
        for &(src, dsts) in edges {
            has_term[src] = true;
            let kind = match dsts {
                [] => InstrKind::Unreachable,
                [dst] => InstrKind::Br { dest: BlockId(*dst as u32) },
                [t, f] => {
                    let cond = ValueRef::Constant(ctx.const_int(ctx.i1_ty, 0));
                    InstrKind::CondBr {
                        cond,
                        then_dest: BlockId(*t as u32),
                        else_dest: BlockId(*f as u32),
                    }
                }
                _ => panic!("at most 2 successors"),
            };
            let iid = func.alloc_instr(Instruction {
                name: None,
                ty: ctx.void_ty,
                kind,
            });
            func.blocks[src].set_terminator(iid);
        }
        // Blocks without an explicitly listed terminator get `Unreachable`.
        for (i, &already) in has_term.iter().enumerate() {
            if !already {
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

    // ── helper: walk the tree and collect all ControlNode variant names ────

    fn collect_variants(node: &ControlNode) -> Vec<&'static str> {
        let mut out = Vec::new();
        collect_variants_inner(node, &mut out);
        out
    }

    fn collect_variants_inner(node: &ControlNode, out: &mut Vec<&'static str>) {
        match node {
            ControlNode::Simple { next, .. } => {
                out.push("Simple");
                if let Some(n) = next {
                    collect_variants_inner(n, out);
                }
            }
            ControlNode::Loop { body, next, .. } => {
                out.push("Loop");
                collect_variants_inner(body, out);
                if let Some(n) = next {
                    collect_variants_inner(n, out);
                }
            }
            ControlNode::Branch { then_node, else_node, next, .. } => {
                out.push("Branch");
                collect_variants_inner(then_node, out);
                if let Some(e) = else_node {
                    collect_variants_inner(e, out);
                }
                if let Some(n) = next {
                    collect_variants_inner(n, out);
                }
            }
        }
    }

    // ── helper: does the tree contain a Loop node with the given header? ──

    fn has_loop_header(node: &ControlNode, header: BlockId) -> bool {
        match node {
            ControlNode::Loop { header: h, body, next } => {
                if *h == header {
                    return true;
                }
                has_loop_header(body, header)
                    || next.as_deref().map_or(false, |n| has_loop_header(n, header))
            }
            ControlNode::Simple { next, .. } => {
                next.as_deref().map_or(false, |n| has_loop_header(n, header))
            }
            ControlNode::Branch { then_node, else_node, next, .. } => {
                has_loop_header(then_node, header)
                    || else_node.as_deref().map_or(false, |n| has_loop_header(n, header))
                    || next.as_deref().map_or(false, |n| has_loop_header(n, header))
            }
        }
    }

    // ── helper: does the tree contain a Branch node at a given block? ─────

    fn has_branch_at(node: &ControlNode, cond_block: BlockId) -> bool {
        match node {
            ControlNode::Branch { cond_block: cb, then_node, else_node, next } => {
                if *cb == cond_block {
                    return true;
                }
                has_branch_at(then_node, cond_block)
                    || else_node.as_deref().map_or(false, |n| has_branch_at(n, cond_block))
                    || next.as_deref().map_or(false, |n| has_branch_at(n, cond_block))
            }
            ControlNode::Simple { next, .. } => {
                next.as_deref().map_or(false, |n| has_branch_at(n, cond_block))
            }
            ControlNode::Loop { body, next, .. } => {
                has_branch_at(body, cond_block)
                    || next.as_deref().map_or(false, |n| has_branch_at(n, cond_block))
            }
        }
    }

    // ── test 1: single block → Simple { id: BlockId(0), next: None } ──────

    #[test]
    fn single_block_returns_simple_node() {
        let (_ctx, func) = build_func(1, &[(0, &[])]);
        let tree = build_control_tree(&func);
        match &tree {
            ControlNode::Simple { id, next } => {
                assert_eq!(*id, BlockId(0));
                assert!(next.is_none(), "single block must have no next");
            }
            other => panic!("expected Simple, got {:?}", other),
        }
    }

    // ── test 2: linear A→B→C → Simple(A, Simple(B, Simple(C))) ───────────

    #[test]
    fn linear_chain_a_to_b_to_c() {
        // Blocks: 0→1→2 (linear chain, no branches)
        let (_ctx, func) = build_func(3, &[(0, &[1]), (1, &[2]), (2, &[])]);
        let tree = build_control_tree(&func);
        let variants = collect_variants(&tree);
        // Must be Simple, Simple, Simple in that order.
        assert_eq!(
            variants,
            vec!["Simple", "Simple", "Simple"],
            "linear chain must produce three Simple nodes"
        );
        // First node must be block 0.
        match &tree {
            ControlNode::Simple { id, .. } => assert_eq!(*id, BlockId(0)),
            _ => panic!("expected Simple at root"),
        }
    }

    // ── test 3: while loop → contains Loop node ───────────────────────────
    //
    // CFG: 0→1→2→1 (back-edge 2→1; 1 is loop header)

    #[test]
    fn while_loop_detected_as_loop_node() {
        let (_ctx, func) = build_func(3, &[(0, &[1]), (1, &[2]), (2, &[1])]);
        let tree = build_control_tree(&func);
        assert!(
            has_loop_header(&tree, BlockId(1)),
            "loop header must be BlockId(1); tree = {:?}",
            tree
        );
    }

    // ── test 4: if/else → Branch node ─────────────────────────────────────
    //
    // CFG: 0 splits to 1 and 2; both go to 3 (diamond shape)

    #[test]
    fn if_else_detected_as_branch_node() {
        let (_ctx, func) =
            build_func(4, &[(0, &[1, 2]), (1, &[3]), (2, &[3]), (3, &[])]);
        let tree = build_control_tree(&func);
        assert!(
            has_branch_at(&tree, BlockId(0)),
            "cond_block must be BlockId(0); tree = {:?}",
            tree
        );
    }

    // ── test 5: loop with exit ─────────────────────────────────────────────
    //
    // CFG: 0→1(header)→2(body)→{1(continue),3(exit)}; 3 is after the loop.

    #[test]
    fn loop_with_exit_has_correct_structure() {
        let (_ctx, func) = build_func(
            4,
            &[(0, &[1]), (1, &[2]), (2, &[1, 3]), (3, &[])],
        );
        let tree = build_control_tree(&func);
        // Tree must contain a Loop node with header=1.
        assert!(
            has_loop_header(&tree, BlockId(1)),
            "must detect loop at header 1; tree = {:?}",
            tree
        );
        // After the loop there must be a continuation (BlockId(3)).
        fn has_simple_3(node: &ControlNode) -> bool {
            match node {
                ControlNode::Simple { id, next } => {
                    if *id == BlockId(3) { return true; }
                    next.as_deref().map_or(false, has_simple_3)
                }
                ControlNode::Loop { next, body, .. } => {
                    has_simple_3(body)
                        || next.as_deref().map_or(false, has_simple_3)
                }
                ControlNode::Branch { then_node, else_node, next, .. } => {
                    has_simple_3(then_node)
                        || else_node.as_deref().map_or(false, has_simple_3)
                        || next.as_deref().map_or(false, has_simple_3)
                }
            }
        }
        assert!(
            has_simple_3(&tree),
            "exit block 3 must appear somewhere in the tree; tree = {:?}",
            tree
        );
    }

    // ── test 6: nested loops ───────────────────────────────────────────────
    //
    // CFG: 0→1(outer header)→2(inner header)→3→{2(inner back),1(outer back)}
    // Outer loop header = 1, inner loop header = 2.

    #[test]
    fn nested_loops_produce_nested_loop_nodes() {
        let (_ctx, func) = build_func(
            4,
            &[(0, &[1]), (1, &[2]), (2, &[3]), (3, &[2, 1])],
        );
        let tree = build_control_tree(&func);
        // Must contain a Loop node at header 1 AND at header 2.
        assert!(
            has_loop_header(&tree, BlockId(1)),
            "outer loop header 1 must be detected; tree = {:?}",
            tree
        );
        assert!(
            has_loop_header(&tree, BlockId(2)),
            "inner loop header 2 must be detected; tree = {:?}",
            tree
        );
    }

    // ── test 7: br_table threshold — dense ≥3 cases ───────────────────────

    #[test]
    fn br_table_threshold_3_dense_cases() {
        // [0, 1, 2]: count=3, min=0, max=2 → max−min=2 < 2*3=6 → true
        assert!(should_use_br_table(&[0, 1, 2]));
    }

    // ── test 8: br_table threshold — sparse cases ─────────────────────────

    #[test]
    fn br_table_threshold_sparse_cases() {
        // [0, 100, 200]: count=3, max−min=200, 2*3=6 → 200 < 6 false
        assert!(!should_use_br_table(&[0, 100, 200]));
    }

    // ── test 9: br_table threshold — below minimum count ──────────────────

    #[test]
    fn br_table_below_threshold() {
        // Only 2 cases → false regardless of density.
        assert!(!should_use_br_table(&[0, 1]));
    }
}
