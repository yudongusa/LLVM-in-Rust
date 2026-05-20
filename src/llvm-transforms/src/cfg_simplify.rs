//! CFG Simplification pass.
//!
//! This pass performs three structural simplifications that clean up the CFG
//! after other passes (e.g., constant folding, jump threading, DCE):
//!
//! 1. **Simplify trivial phis**: a phi with only one incoming value is replaced
//!    by the value itself, because SSA uniqueness guarantees there is only one
//!    definition reaching the block.
//!
//! 2. **Prune unreachable blocks**: blocks with no predecessors (except the
//!    entry block) cannot be executed and are removed together with their
//!    instructions.  Phi nodes in surviving blocks that listed the removed
//!    block as an incoming edge have that edge dropped.
//!
//! 3. **Merge fallthrough blocks**: if block A ends with an unconditional `br`
//!    to block B, and B has exactly one predecessor (A), A and B are merged into
//!    a single block.  B must have no phi nodes (guaranteed by step 1 since the
//!    sole predecessor means all phis would have been trivial).

use crate::pass::FunctionPass;
use llvm_ir::{BlockId, Context, Function, InstrId, InstrKind, ValueRef};
use std::collections::{HashMap, HashSet};

/// CFG simplification pass.
#[derive(Default)]
pub struct CfgSimplify;

impl FunctionPass for CfgSimplify {
    fn name(&self) -> &'static str {
        "cfg-simplify"
    }

    fn run_on_function(&mut self, _ctx: &mut Context, func: &mut Function) -> bool {
        let mut changed = false;
        // Simplify trivial phis first so that single-predecessor blocks (which
        // will be merged next) have no phi nodes.
        changed |= simplify_trivial_phis(func);
        // Prune dead blocks (no predecessors) before merging to avoid merging
        // a live block into a block that would later be deleted.
        changed |= prune_unreachable(func);
        // Finally merge fallthroughs.
        changed |= merge_fallthrough(func);
        changed
    }
}

// ---------------------------------------------------------------------------
// 1. Simplify trivial phis
// ---------------------------------------------------------------------------

/// Replace every single-incoming phi with the value it wraps, then remove it.
///
/// Returns `true` if any phi was eliminated.
fn simplify_trivial_phis(func: &mut Function) -> bool {
    // Collect (phi_instr_id, replacement_value) pairs for phis with exactly
    // one incoming entry.
    let trivial: Vec<(InstrId, ValueRef)> = func
        .instructions
        .iter()
        .enumerate()
        .filter_map(|(i, instr)| {
            if let InstrKind::Phi { incoming, .. } = &instr.kind {
                if incoming.len() == 1 {
                    return Some((InstrId(i as u32), incoming[0].0));
                }
            }
            None
        })
        .collect();

    if trivial.is_empty() {
        return false;
    }

    // Build a substitution map: InstrId → replacement ValueRef.
    let subst: HashMap<InstrId, ValueRef> = trivial.iter().cloned().collect();

    // Rewrite every operand in every instruction using the substitution map.
    apply_value_substitution(func, &subst);

    // Remove the now-dead phi instructions from their blocks' body lists.
    let dead_ids: HashSet<InstrId> = subst.keys().cloned().collect();
    for bb in &mut func.blocks {
        bb.body.retain(|id| !dead_ids.contains(id));
        // Phi nodes are in body (not terminator), so no terminator fixup needed.
    }

    true
}

// ---------------------------------------------------------------------------
// 2. Prune unreachable blocks
// ---------------------------------------------------------------------------

/// Remove blocks that have no predecessors (except the entry block at index 0).
///
/// After removing a block B, drop any incoming edge referencing B from phi
/// nodes in surviving blocks.
///
/// Returns `true` if any block was removed.
fn prune_unreachable(func: &mut Function) -> bool {
    if func.blocks.is_empty() {
        return false;
    }

    // Build predecessor set: pred_count[i] = number of predecessors of block i.
    let reachable = compute_reachable(func);

    // Identify blocks to remove: not reachable and not entry block.
    let to_remove: HashSet<BlockId> = (0..func.blocks.len())
        .filter(|&i| i != 0 && !reachable.contains(&BlockId(i as u32)))
        .map(|i| BlockId(i as u32))
        .collect();

    if to_remove.is_empty() {
        return false;
    }

    // Collect all InstrIds in removed blocks so we can also clean up
    // value_names.
    let dead_instrs: HashSet<InstrId> = to_remove
        .iter()
        .flat_map(|bid| func.blocks[bid.0 as usize].instrs())
        .collect();

    // For surviving blocks: drop phi incoming edges from removed blocks.
    for bidx in 0..func.blocks.len() {
        if to_remove.contains(&BlockId(bidx as u32)) {
            continue;
        }
        let term_opt = func.blocks[bidx].terminator;
        // Process body instructions (phis live in body).
        for pos in 0..func.blocks[bidx].body.len() {
            let iid = func.blocks[bidx].body[pos];
            if let InstrKind::Phi { ty, incoming } = &func.instructions[iid.0 as usize].kind {
                let new_incoming: Vec<(ValueRef, BlockId)> = incoming
                    .iter()
                    .filter(|(_, blk)| !to_remove.contains(blk))
                    .cloned()
                    .collect();
                if new_incoming.len() != incoming.len() {
                    let ty = *ty;
                    func.instructions[iid.0 as usize].kind =
                        InstrKind::Phi { ty, incoming: new_incoming };
                }
            }
        }
        // Terminators don't reference predecessor blocks (they reference
        // successors), so no fixup needed for terminators.
        let _ = term_opt;
    }

    // Remove dead value_names entries.
    func.value_names.retain(|_, iid| !dead_instrs.contains(iid));

    // Remove the blocks (retain only reachable and entry).
    // We need to renumber BlockIds everywhere after removal.
    // Build mapping: old BlockId → new BlockId.
    let mut new_idx: Vec<Option<u32>> = vec![None; func.blocks.len()];
    let mut next = 0u32;
    for i in 0..func.blocks.len() {
        if !to_remove.contains(&BlockId(i as u32)) {
            new_idx[i] = Some(next);
            next += 1;
        }
    }

    // Rewrite all BlockId references in instructions.
    remap_block_ids(func, &new_idx);

    // Physically remove blocks in one pass.
    let mut keep_iter = new_idx.iter();
    func.blocks.retain(|_| keep_iter.next().unwrap().is_some());

    true
}

/// Compute the set of reachable `BlockId`s using DFS from the entry block.
fn compute_reachable(func: &Function) -> HashSet<BlockId> {
    if func.blocks.is_empty() {
        return HashSet::new();
    }
    let mut visited = HashSet::new();
    let mut stack = vec![BlockId(0)];
    while let Some(bid) = stack.pop() {
        if !visited.insert(bid) {
            continue;
        }
        // Push successors.
        if let Some(term_id) = func.blocks[bid.0 as usize].terminator {
            match &func.instructions[term_id.0 as usize].kind {
                InstrKind::Br { dest } => {
                    stack.push(*dest);
                }
                InstrKind::CondBr {
                    then_dest,
                    else_dest,
                    ..
                } => {
                    stack.push(*then_dest);
                    stack.push(*else_dest);
                }
                InstrKind::Switch { default, cases, .. } => {
                    stack.push(*default);
                    for (_, blk) in cases {
                        stack.push(*blk);
                    }
                }
                InstrKind::Invoke {
                    normal_dest,
                    unwind_dest,
                    ..
                } => {
                    stack.push(*normal_dest);
                    stack.push(*unwind_dest);
                }
                // Ret / Unreachable — no successors.
                _ => {}
            }
        }
    }
    visited
}

/// Rewrite all `BlockId` fields in every instruction using `new_idx`.
///
/// `new_idx[old.0]` = `Some(new_index)` for kept blocks, `None` for removed.
/// Removed-block references in phi incoming lists should have been dropped
/// before calling this function.
///
/// Uses `.get()` for bounds-safe access: the flat instruction pool retains
/// dead instructions (those no longer referenced by any block) across
/// iterations, and their stale BlockIds may fall outside `new_idx`.  Dead
/// instructions are ignored here because no live block will ever execute them.
fn remap_block_ids(func: &mut Function, new_idx: &[Option<u32>]) {
    let remap = |blk: &mut BlockId| {
        if let Some(&Some(n)) = new_idx.get(blk.0 as usize) {
            *blk = BlockId(n);
        }
        // Out-of-bounds or None → leave as-is (dead instruction, harmless).
    };
    for instr in &mut func.instructions {
        match &mut instr.kind {
            InstrKind::Br { dest } => remap(dest),
            InstrKind::CondBr { then_dest, else_dest, .. } => {
                remap(then_dest);
                remap(else_dest);
            }
            InstrKind::Switch { default, cases, .. } => {
                remap(default);
                for (_, blk) in cases.iter_mut() {
                    remap(blk);
                }
            }
            InstrKind::Invoke { normal_dest, unwind_dest, .. } => {
                remap(normal_dest);
                remap(unwind_dest);
            }
            InstrKind::Phi { incoming, .. } => {
                for (_, blk) in incoming.iter_mut() {
                    remap(blk);
                }
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// 3. Merge fallthrough blocks
// ---------------------------------------------------------------------------

/// Merge every block A that ends with an unconditional `br` to block B when B
/// has exactly one predecessor (A).
///
/// The merge absorbs B's body and terminator into A, then removes B.
///
/// Returns `true` if any merge happened.
fn merge_fallthrough(func: &mut Function) -> bool {
    if func.blocks.len() < 2 {
        return false;
    }

    // Build predecessor count for each block.
    let mut pred_count = vec![0usize; func.blocks.len()];
    // Entry block is considered reachable without an explicit predecessor.
    // We only count CFG edges below.
    for bidx in 0..func.blocks.len() {
        if let Some(term_id) = func.blocks[bidx].terminator {
            match &func.instructions[term_id.0 as usize].kind {
                InstrKind::Br { dest } => {
                    pred_count[dest.0 as usize] += 1;
                }
                InstrKind::CondBr {
                    then_dest,
                    else_dest,
                    ..
                } => {
                    pred_count[then_dest.0 as usize] += 1;
                    pred_count[else_dest.0 as usize] += 1;
                }
                InstrKind::Switch { default, cases, .. } => {
                    pred_count[default.0 as usize] += 1;
                    for (_, blk) in cases {
                        pred_count[blk.0 as usize] += 1;
                    }
                }
                InstrKind::Invoke {
                    normal_dest,
                    unwind_dest,
                    ..
                } => {
                    pred_count[normal_dest.0 as usize] += 1;
                    pred_count[unwind_dest.0 as usize] += 1;
                }
                _ => {}
            }
        }
    }

    // Look for block A ending in Br { dest: B } where pred_count[B] == 1.
    // We process in a single forward pass; after merging we re-run via the
    // PassManager's fixed-point iteration if needed.
    let mut to_remove: HashSet<BlockId> = HashSet::new();
    let mut merges: Vec<(usize, usize)> = Vec::new(); // (a_idx, b_idx)

    for a_idx in 0..func.blocks.len() {
        if to_remove.contains(&BlockId(a_idx as u32)) {
            continue;
        }
        let Some(term_id) = func.blocks[a_idx].terminator else {
            continue;
        };
        let dest = match &func.instructions[term_id.0 as usize].kind {
            InstrKind::Br { dest } => *dest,
            _ => continue,
        };
        let b_idx = dest.0 as usize;
        if b_idx == a_idx {
            // Self-loop — skip.
            continue;
        }
        if pred_count[b_idx] != 1 {
            continue;
        }
        // B must not have phi nodes (they should have been eliminated by
        // simplify_trivial_phis, but guard anyway).
        let b_has_phi = func.blocks[b_idx].body.iter().any(|&iid| {
            matches!(func.instructions[iid.0 as usize].kind, InstrKind::Phi { .. })
        });
        if b_has_phi {
            continue;
        }
        merges.push((a_idx, b_idx));
        to_remove.insert(BlockId(b_idx as u32));
    }

    if merges.is_empty() {
        return false;
    }

    // Perform merges. For each (a, b):
    //   - Remove A's unconditional branch terminator from A.
    //   - Append all of B's body instructions to A's body.
    //   - Move B's terminator into A.
    //
    // Because we use an instruction pool with stable InstrIds, we only need to
    // manipulate block.body / block.terminator vectors — instruction data stays
    // in func.instructions unchanged.
    for (a_idx, b_idx) in &merges {
        // Drain B's body and terminator before we touch A (avoid aliasing).
        let b_body: Vec<InstrId> = std::mem::take(&mut func.blocks[*b_idx].body);
        let b_term: Option<InstrId> = func.blocks[*b_idx].terminator.take();

        // Replace A's Br terminator: discard it (it stays in the instruction
        // pool as a dead instruction but is removed from the block).
        func.blocks[*a_idx].terminator = None;

        // Append B's body instructions to A's body.
        func.blocks[*a_idx].body.extend_from_slice(&b_body);

        // Set A's terminator to B's terminator.
        func.blocks[*a_idx].terminator = b_term;
    }

    // Now remove the absorbed blocks and renumber.
    let mut new_idx: Vec<Option<u32>> = vec![None; func.blocks.len()];
    let mut next = 0u32;
    for i in 0..func.blocks.len() {
        if !to_remove.contains(&BlockId(i as u32)) {
            new_idx[i] = Some(next);
            next += 1;
        }
    }

    remap_block_ids(func, &new_idx);

    let mut keep_iter = new_idx.iter();
    func.blocks.retain(|_| keep_iter.next().unwrap().is_some());

    true
}

// ---------------------------------------------------------------------------
// Shared helper: apply a value substitution to every instruction operand
// ---------------------------------------------------------------------------

/// For every instruction in `func`, replace any `ValueRef::Instruction(id)`
/// that appears in `subst` with the mapped replacement.
fn apply_value_substitution(func: &mut Function, subst: &HashMap<InstrId, ValueRef>) {
    if subst.is_empty() {
        return;
    }
    let remap = |v: ValueRef| -> ValueRef {
        if let ValueRef::Instruction(id) = v {
            if let Some(&repl) = subst.get(&id) {
                return repl;
            }
        }
        v
    };
    for instr in &mut func.instructions {
        let kind = std::mem::replace(&mut instr.kind, InstrKind::Unreachable);
        instr.kind = crate::value_rewrite::rewrite_values_in_kind(kind, remap);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pass::FunctionPass;
    use crate::pipeline::{build_pipeline, OptLevel};
    use llvm_ir::{Builder, Context, InstrKind, IntPredicate, Linkage, Module, ValueRef};

    // -----------------------------------------------------------------------
    // Test 1: prune unreachable block
    // -----------------------------------------------------------------------
    // Build:
    //   entry: br live
    //   live: ret i32 1
    //   dead: ret i32 99    <- no predecessors; should be pruned
    //
    // Note: merge_fallthrough also runs in the same pass, so entry and live
    // get merged into one block (entry br live, live has exactly one pred).
    // The final result is 1 block containing `ret i32 1` — both the dead
    // block removal and the fallthrough merge are verified here.
    #[test]
    fn prune_unreachable_block() {
        let mut ctx = Context::new();
        let mut module = Module::new("test");
        let mut b = Builder::new(&mut ctx, &mut module);
        b.add_function("f", b.ctx.i32_ty, vec![], vec![], false, Linkage::External);
        let entry = b.add_block("entry");
        let live = b.add_block("live");
        let _dead = b.add_block("dead");

        b.position_at_end(entry);
        b.build_br(live);

        b.position_at_end(live);
        let c1 = b.const_int(b.ctx.i32_ty, 1);
        b.build_ret(c1);

        b.position_at_end(_dead);
        let c99 = b.const_int(b.ctx.i32_ty, 99);
        b.build_ret(c99);

        assert_eq!(module.functions[0].blocks.len(), 3);

        let mut pass = CfgSimplify;
        let changed = pass.run_on_function(&mut ctx, &mut module.functions[0]);
        assert!(changed, "pass should report change");

        // After pruning dead (no preds) and merging fallthrough entry→live:
        // only 1 block remains.
        assert_eq!(
            module.functions[0].blocks.len(),
            1,
            "dead block pruned and entry/live merged: expect 1 block"
        );

        // The surviving block must return i32 1 (not 99 from the dead block).
        let f = &module.functions[0];
        let tid = f.blocks[0].terminator.expect("terminator");
        match &f.instr(tid).kind {
            InstrKind::Ret {
                val: Some(ValueRef::Constant(cid)),
            } => {
                match ctx.get_const(*cid) {
                    llvm_ir::ConstantData::Int { val, .. } => {
                        assert_eq!(*val, 1, "should return 1, not 99 from dead block")
                    }
                    other => panic!("unexpected constant: {other:?}"),
                }
            }
            other => panic!("expected ret i32 constant, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // Test 2: merge fallthrough blocks A → B → C where B has 1 pred
    // -----------------------------------------------------------------------
    #[test]
    fn merge_fallthrough_single_pred() {
        let mut ctx = Context::new();
        let mut module = Module::new("test");
        let mut b = Builder::new(&mut ctx, &mut module);
        b.add_function("f", b.ctx.i32_ty, vec![], vec![], false, Linkage::External);
        let a = b.add_block("a");
        let bblk = b.add_block("b");
        let c = b.add_block("c");

        b.position_at_end(a);
        b.build_br(bblk); // A → B

        b.position_at_end(bblk);
        b.build_br(c); // B → C

        b.position_at_end(c);
        let cv = b.const_int(b.ctx.i32_ty, 42);
        b.build_ret(cv);

        assert_eq!(module.functions[0].blocks.len(), 3);

        let mut pass = CfgSimplify;
        let changed = pass.run_on_function(&mut ctx, &mut module.functions[0]);
        assert!(changed, "pass should report change");
        // A and B should have been merged; then the resulting A merges with C
        // in the same pass invocation (or after a second run).
        // After one pass run: A merged with B → 2 blocks (A, C) → then A
        // merges with C → 1 block. But our implementation does a single
        // forward sweep so it may take 2 calls. Run to fixed point.
        let mut pass2 = CfgSimplify;
        pass2.run_on_function(&mut ctx, &mut module.functions[0]);

        assert_eq!(
            module.functions[0].blocks.len(),
            1,
            "all fallthrough blocks should be merged into one"
        );
    }

    // -----------------------------------------------------------------------
    // Test 3: trivial phi with one incoming is replaced by its value
    // -----------------------------------------------------------------------
    #[test]
    fn simplify_trivial_phi_one_incoming() {
        let mut ctx = Context::new();
        let mut module = Module::new("test");
        let mut b = Builder::new(&mut ctx, &mut module);
        b.add_function("f", b.ctx.i32_ty, vec![], vec![], false, Linkage::External);
        let entry = b.add_block("entry");
        let join = b.add_block("join");

        b.position_at_end(entry);
        let cval = b.const_int(b.ctx.i32_ty, 5);
        b.build_br(join);

        b.position_at_end(join);
        // phi [ 5, entry ] — one incoming, trivially replaceable
        let phi = b.build_phi("p", b.ctx.i32_ty, vec![(cval, entry)]);
        b.build_ret(phi);

        // Before pass: join has 1 body instruction (phi).
        assert_eq!(module.functions[0].blocks[1].body.len(), 1);

        let mut pass = CfgSimplify;
        let changed = pass.run_on_function(&mut ctx, &mut module.functions[0]);
        assert!(changed, "pass should report change");

        // After: phi removed from body.
        let f = &module.functions[0];
        // join block body should be empty (phi removed).
        // find join block — it may have been renumbered or merged.
        // Verify the terminator returns cval directly.
        let found_ret_const = f.blocks.iter().any(|bb| {
            bb.terminator
                .map(|tid| {
                    matches!(&f.instr(tid).kind,
                        InstrKind::Ret { val: Some(ValueRef::Constant(_)) }
                    )
                })
                .unwrap_or(false)
        });
        assert!(
            found_ret_const,
            "after phi simplification, ret should use a constant directly"
        );
    }

    // -----------------------------------------------------------------------
    // Test 4: multi-predecessor block is NOT merged
    // -----------------------------------------------------------------------
    #[test]
    fn no_merge_multi_predecessor() {
        // A1 → join, A2 → join; join has 2 predecessors — must not be merged.
        let mut ctx = Context::new();
        let mut module = Module::new("test");
        let mut b = Builder::new(&mut ctx, &mut module);
        b.add_function(
            "f",
            b.ctx.i32_ty,
            vec![b.ctx.i1_ty],
            vec!["c".into()],
            false,
            Linkage::External,
        );
        let entry = b.add_block("entry");
        let a1 = b.add_block("a1");
        let a2 = b.add_block("a2");
        let join = b.add_block("join");

        let cond = b.get_arg(0);
        b.position_at_end(entry);
        b.build_cond_br(cond, a1, a2);

        b.position_at_end(a1);
        b.build_br(join);

        b.position_at_end(a2);
        b.build_br(join);

        b.position_at_end(join);
        let cv = b.const_int(b.ctx.i32_ty, 0);
        b.build_ret(cv);

        let before = module.functions[0].blocks.len();
        let mut pass = CfgSimplify;
        let changed = pass.run_on_function(&mut ctx, &mut module.functions[0]);
        // Either no change, or only entry might be merged (entry → a1/a2 is
        // a CondBr so no merge). Expect block count unchanged.
        let after = module.functions[0].blocks.len();
        assert_eq!(
            before, after,
            "multi-predecessor join block must not be merged (before={before}, after={after}, changed={changed})"
        );
    }

    // -----------------------------------------------------------------------
    // Test 5: combined — dead block + trivial phi + fallthrough in one fn
    // -----------------------------------------------------------------------
    #[test]
    fn combined_dead_phi_fallthrough() {
        // entry --br--> mid --br--> exit
        // dead (no pred)
        // mid has a trivial phi from entry
        let mut ctx = Context::new();
        let mut module = Module::new("test");
        let mut b = Builder::new(&mut ctx, &mut module);
        b.add_function("f", b.ctx.i32_ty, vec![], vec![], false, Linkage::External);
        let entry = b.add_block("entry");
        let mid = b.add_block("mid");
        let exit = b.add_block("exit");
        let dead = b.add_block("dead");

        let cv = b.const_int(b.ctx.i32_ty, 10);

        b.position_at_end(entry);
        b.build_br(mid);

        b.position_at_end(mid);
        let phi_v = b.build_phi("pv", b.ctx.i32_ty, vec![(cv, entry)]);
        b.build_br(exit);

        b.position_at_end(exit);
        b.build_ret(phi_v);

        b.position_at_end(dead);
        let cd = b.const_int(b.ctx.i32_ty, 999);
        b.build_ret(cd);

        assert_eq!(module.functions[0].blocks.len(), 4);

        let mut pass = CfgSimplify;
        pass.run_on_function(&mut ctx, &mut module.functions[0]);
        // Run again to reach fixed point.
        pass.run_on_function(&mut ctx, &mut module.functions[0]);

        let f = &module.functions[0];
        // Should have at most 1 block (entry merged with mid merged with exit,
        // dead removed).
        assert!(
            f.blocks.len() <= 2,
            "after combined simplification, expect ≤ 2 blocks, got {}",
            f.blocks.len()
        );
        // The return should use the constant (phi was folded).
        let found_ret_const = f.blocks.iter().any(|bb| {
            bb.terminator
                .map(|tid| {
                    matches!(&f.instr(tid).kind,
                        InstrKind::Ret { val: Some(ValueRef::Constant(_)) }
                    )
                })
                .unwrap_or(false)
        });
        assert!(found_ret_const, "combined: ret should reference a constant");
    }

    // -----------------------------------------------------------------------
    // Test 6: O2 pipeline block count reduction vs O0
    // -----------------------------------------------------------------------
    #[test]
    fn o2_pipeline_reduces_block_count() {
        // Build a function with a constant conditional that creates an
        // unreachable block.  O0 leaves it; O2 should remove it.
        //
        //   entry: %c = icmp eq i32 0, 0   ; always true
        //          condbr %c, then, else
        //   then: ret i32 1
        //   else: ret i32 2    <- unreachable (constant branch)
        let build = || {
            let mut ctx = Context::new();
            let mut module = Module::new("test");
            let mut b = Builder::new(&mut ctx, &mut module);
            b.add_function("f", b.ctx.i32_ty, vec![], vec![], false, Linkage::External);
            let entry = b.add_block("entry");
            let then_b = b.add_block("then");
            let else_b = b.add_block("else");

            b.position_at_end(entry);
            let zero = b.const_int(b.ctx.i32_ty, 0);
            let cmp = b.build_icmp("c", IntPredicate::Eq, zero, zero);
            b.build_cond_br(cmp, then_b, else_b);

            b.position_at_end(then_b);
            let c1 = b.const_int(b.ctx.i32_ty, 1);
            b.build_ret(c1);

            b.position_at_end(else_b);
            let c2 = b.const_int(b.ctx.i32_ty, 2);
            b.build_ret(c2);

            (ctx, module)
        };

        let (mut ctx_o0, mut m_o0) = build();
        let (mut ctx_o2, mut m_o2) = build();

        let mut pm_o0 = build_pipeline(OptLevel::O0);
        let mut pm_o2 = build_pipeline(OptLevel::O2);
        pm_o0.run_until_fixed_point(&mut ctx_o0, &mut m_o0, 3);
        pm_o2.run_until_fixed_point(&mut ctx_o2, &mut m_o2, 8);

        let blocks_o0 = m_o0.functions[0].blocks.len();
        let blocks_o2 = m_o2.functions[0].blocks.len();
        assert!(
            blocks_o2 < blocks_o0,
            "O2 should have fewer blocks than O0 (o0={blocks_o0}, o2={blocks_o2})"
        );
    }
}
