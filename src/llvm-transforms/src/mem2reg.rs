//! mem2reg: promotes scalar alloca/load/store triples to SSA values.
//!
//! Implements the Cytron et al. (1991) algorithm:
//!
//! 1. **Identify promotable allocas** — scalar allocas in the entry block
//!    whose address is only ever used as the operand of non-volatile
//!    `load`/`store` instructions.
//! 2. **Insert φ-nodes** at the iterated dominance frontier (IDF) of every
//!    block that stores to the alloca.
//! 3. **Rename** — walk the dominator tree keeping a "current definition"
//!    stack per alloca.  Each `store` pushes a new definition; each `load`
//!    is replaced by the top of the stack; φ-nodes are filled in with the
//!    reaching definition from each predecessor.
//!
//! After the pass, all promoted allocas, their loads, and their stores are
//! removed.  Non-promotable memory operations are left unchanged.

use crate::const_prop::subst_kind;
use crate::pass::FunctionPass;
use llvm_analysis::{Cfg, DomTree};
use llvm_ir::{BlockId, Context, Function, InstrId, InstrKind, Instruction, TypeId, ValueRef};
use std::collections::{HashMap, HashSet, VecDeque};

/// mem2reg pass.
pub struct Mem2Reg;

impl FunctionPass for Mem2Reg {
    fn name(&self) -> &'static str {
        "mem2reg"
    }

    fn run_on_function(&mut self, ctx: &mut Context, func: &mut Function) -> bool {
        if func.blocks.is_empty() {
            return false;
        }

        let promotable = find_promotable_allocas(func);
        if promotable.is_empty() {
            return false;
        }

        let cfg = Cfg::compute(func);
        let dom = DomTree::compute(func, &cfg);
        let df = dom.dominance_frontier(&cfg);

        // Step 2 — insert phi nodes at the IDF of each alloca's def sites.
        let phi_map = insert_phis(ctx, func, &promotable, &df, &cfg);

        // Step 3 — rename: walk the dominator tree collecting substitutions.
        let mut subst: HashMap<InstrId, ValueRef> = HashMap::new();
        let mut instrs_to_remove: HashSet<InstrId> = HashSet::new();
        let mut phi_updates: Vec<(InstrId, BlockId, ValueRef)> = Vec::new();

        // Initialise stacks: each alloca starts with an `undef`.
        let mut stacks: HashMap<InstrId, Vec<ValueRef>> = promotable
            .iter()
            .map(|(&iid, &ty)| (iid, vec![ValueRef::Constant(ctx.const_undef(ty))]))
            .collect();

        // Build dominator-children list.
        let n = func.num_blocks();
        let mut dom_children: Vec<Vec<BlockId>> = vec![Vec::new(); n];
        for bi in 0..n {
            let bid = BlockId(bi as u32);
            if let Some(idom) = dom.idom(bid) {
                dom_children[idom.0 as usize].push(bid);
            }
        }

        rename_dfs(
            BlockId(0),
            func,
            &promotable,
            &phi_map,
            &cfg,
            &dom_children,
            &mut stacks,
            &mut subst,
            &mut instrs_to_remove,
            &mut phi_updates,
        );

        // Apply phi incoming-value updates collected during the rename.
        for (phi_iid, pred, new_val) in phi_updates {
            if let InstrKind::Phi {
                ref mut incoming, ..
            } = func.instr_mut(phi_iid).kind
            {
                for (val, blk) in incoming.iter_mut() {
                    if *blk == pred {
                        *val = new_val;
                        break;
                    }
                }
            }
        }

        // Apply load-substitution to all remaining instructions.
        if !subst.is_empty() {
            for instr in func.instructions.iter_mut() {
                let new_kind = subst_kind(instr.kind.clone(), &subst);
                instr.kind = new_kind;
            }
        }

        // Remove promoted allocas, loads, and stores from block bodies.
        instrs_to_remove.extend(promotable.keys().copied());
        for bb in &mut func.blocks {
            bb.body.retain(|id| !instrs_to_remove.contains(id));
        }

        true
    }
}

// ---------------------------------------------------------------------------
// Step 1 — find promotable allocas
// ---------------------------------------------------------------------------

/// An alloca is **promotable** iff:
/// - It lives in the entry block (BlockId(0)).
/// - It has no `num_elements` (scalar alloca, not `alloca T, N`).
/// - Every use of its address is a non-volatile `load ptr` or
///   `store val, ptr` (address never captured or passed elsewhere).
fn find_promotable_allocas(func: &Function) -> HashMap<InstrId, TypeId> {
    let mut result = HashMap::new();
    let entry = match func.blocks.first() {
        Some(b) => b,
        None => return result,
    };

    let alloca_iids: Vec<InstrId> = entry
        .body
        .iter()
        .copied()
        .filter(|&iid| {
            matches!(
                func.instr(iid).kind,
                InstrKind::Alloca {
                    num_elements: None,
                    ..
                }
            )
        })
        .collect();

    'outer: for &alloca_iid in &alloca_iids {
        let alloc_ty = match func.instr(alloca_iid).kind {
            InstrKind::Alloca { alloc_ty, .. } => alloc_ty,
            _ => unreachable!(),
        };
        let ptr = ValueRef::Instruction(alloca_iid);

        for instr in &func.instructions {
            match &instr.kind {
                // Non-volatile load from the alloca address — OK.
                InstrKind::Load {
                    ptr: p,
                    volatile: false,
                    ..
                } if *p == ptr => {}
                // Non-volatile store of a non-self value to the alloca address — OK.
                InstrKind::Store {
                    ptr: p,
                    val,
                    volatile: false,
                    ..
                } if *p == ptr => {
                    // Don't promote if the alloca's address is stored through itself.
                    if *val == ptr {
                        continue 'outer;
                    }
                }
                // Any other use of the alloca's address → not promotable.
                kind if kind.operands().contains(&ptr) => continue 'outer,
                _ => {}
            }
        }

        result.insert(alloca_iid, alloc_ty);
    }

    result
}

// ---------------------------------------------------------------------------
// Step 2 — phi insertion
// ---------------------------------------------------------------------------

/// For each promotable alloca, compute its IDF and insert a phi in each
/// block of the IDF.  Returns a map `BlockId → [(alloca_iid, phi_iid)]`.
fn insert_phis(
    ctx: &mut Context,
    func: &mut Function,
    promotable: &HashMap<InstrId, TypeId>,
    df: &HashMap<BlockId, Vec<BlockId>>,
    cfg: &Cfg,
) -> HashMap<BlockId, Vec<(InstrId, InstrId)>> {
    let mut phi_map: HashMap<BlockId, Vec<(InstrId, InstrId)>> = HashMap::new();

    for (&alloca_iid, &alloc_ty) in promotable {
        let def_blocks = find_def_blocks(func, alloca_iid);
        let idf = iterated_df(&def_blocks, df);

        for block_id in idf {
            let preds = cfg.predecessors(block_id);
            let undef = ctx.const_undef(alloc_ty);
            let incoming: Vec<(ValueRef, BlockId)> = preds
                .iter()
                .map(|&p| (ValueRef::Constant(undef), p))
                .collect();

            let phi_name = func.fresh_name();
            let phi_iid = func.alloc_instr(Instruction {
                name: Some(phi_name),
                ty: alloc_ty,
                kind: InstrKind::Phi {
                    ty: alloc_ty,
                    incoming,
                },
            });
            // Phi nodes go at the front of the block body.
            func.blocks[block_id.0 as usize].body.insert(0, phi_iid);
            phi_map
                .entry(block_id)
                .or_default()
                .push((alloca_iid, phi_iid));
        }
    }

    phi_map
}

fn find_def_blocks(func: &Function, alloca_iid: InstrId) -> Vec<BlockId> {
    let ptr = ValueRef::Instruction(alloca_iid);
    let mut result = Vec::new();
    for (bi, bb) in func.blocks.iter().enumerate() {
        for iid in bb.instrs() {
            if let InstrKind::Store { ptr: p, .. } = &func.instr(iid).kind {
                if *p == ptr {
                    result.push(BlockId(bi as u32));
                    break;
                }
            }
        }
    }
    result
}

fn iterated_df(def_blocks: &[BlockId], df: &HashMap<BlockId, Vec<BlockId>>) -> Vec<BlockId> {
    let mut in_idf: HashSet<BlockId> = HashSet::new();
    let mut worklist: VecDeque<BlockId> = def_blocks.iter().copied().collect();
    while let Some(b) = worklist.pop_front() {
        for &y in df.get(&b).map(|v| v.as_slice()).unwrap_or(&[]) {
            if in_idf.insert(y) {
                worklist.push_back(y);
            }
        }
    }
    in_idf.into_iter().collect()
}

// ---------------------------------------------------------------------------
// Step 3 — rename (iterative DFS over dominator tree)
// ---------------------------------------------------------------------------

/// Rename all loads and stores for the promotable allocas in the subtree
/// rooted at `block` in the dominator tree.
#[allow(clippy::too_many_arguments)]
fn rename_dfs(
    block: BlockId,
    func: &mut Function,
    promotable: &HashMap<InstrId, TypeId>,
    phi_map: &HashMap<BlockId, Vec<(InstrId, InstrId)>>,
    cfg: &Cfg,
    dom_children: &[Vec<BlockId>],
    stacks: &mut HashMap<InstrId, Vec<ValueRef>>,
    subst: &mut HashMap<InstrId, ValueRef>,
    instrs_to_remove: &mut HashSet<InstrId>,
    phi_updates: &mut Vec<(InstrId, BlockId, ValueRef)>,
) {
    // Track stack heights so we can undo pushes when we leave this block.
    let mut saved: Vec<(InstrId, usize)> = Vec::new();

    // Push the phi defined in this block as the new reaching def.
    if let Some(phis) = phi_map.get(&block) {
        for &(alloca_iid, phi_iid) in phis {
            let stack = stacks.get_mut(&alloca_iid).unwrap();
            saved.push((alloca_iid, stack.len()));
            stack.push(ValueRef::Instruction(phi_iid));
        }
    }

    // Process non-terminator instructions.
    let body: Vec<InstrId> = func.blocks[block.0 as usize].body.clone();
    for iid in body {
        match func.instr(iid).kind.clone() {
            InstrKind::Alloca { .. } if promotable.contains_key(&iid) => {
                // The alloca itself — nothing to push; removal handled later.
            }
            InstrKind::Load {
                ptr: ValueRef::Instruction(alloca_iid),
                volatile: false,
                ..
            } => {
                if let Some(stack) = stacks.get(&alloca_iid) {
                    let def = *stack.last().unwrap();
                    subst.insert(iid, def);
                    instrs_to_remove.insert(iid);
                }
            }
            InstrKind::Store {
                val,
                ptr: ValueRef::Instruction(alloca_iid),
                volatile: false,
                ..
            } => {
                if stacks.contains_key(&alloca_iid) {
                    // If the stored value is itself a replaced load, resolve it.
                    let resolved = if let ValueRef::Instruction(vid) = val {
                        subst.get(&vid).copied().unwrap_or(val)
                    } else {
                        val
                    };
                    let stack = stacks.get_mut(&alloca_iid).unwrap();
                    saved.push((alloca_iid, stack.len()));
                    stack.push(resolved);
                    instrs_to_remove.insert(iid);
                }
            }
            _ => {}
        }
    }

    // Record updates for phi incoming values in successor blocks.
    for &succ in cfg.successors(block) {
        if let Some(phis) = phi_map.get(&succ) {
            for &(alloca_iid, phi_iid) in phis {
                let def = *stacks[&alloca_iid].last().unwrap();
                phi_updates.push((phi_iid, block, def));
            }
        }
    }

    // Recurse into dominator-tree children.
    let children = dom_children[block.0 as usize].clone();
    for child in children {
        rename_dfs(
            child,
            func,
            promotable,
            phi_map,
            cfg,
            dom_children,
            stacks,
            subst,
            instrs_to_remove,
            phi_updates,
        );
    }

    // Undo stack pushes made in this block (restore caller's view).
    for (alloca_iid, saved_len) in saved.into_iter().rev() {
        stacks.get_mut(&alloca_iid).unwrap().truncate(saved_len);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pass::FunctionPass;
    use llvm_ir::{Builder, Context, Linkage, Module, ValueRef};

    // Build:  f(i32 %x) -> i32 {
    //   entry: %p = alloca i32
    //          store %x, %p
    //          %v = load i32, %p
    //          ret %v
    // }
    // After mem2reg: alloca/store/load removed, ret directly uses %x.
    fn make_simple_fn() -> (Context, Module) {
        let mut ctx = Context::new();
        let mut module = Module::new("test");
        let mut b = Builder::new(&mut ctx, &mut module);
        b.add_function(
            "f",
            b.ctx.i32_ty,
            vec![b.ctx.i32_ty],
            vec!["x".into()],
            false,
            Linkage::External,
        );
        let entry = b.add_block("entry");
        b.position_at_end(entry);
        let x = b.get_arg(0);
        let p = b.build_alloca("p", b.ctx.i32_ty);
        b.build_store(x, p);
        let v = b.build_load("v", b.ctx.i32_ty, p);
        b.build_ret(v);
        (ctx, module)
    }

    #[test]
    fn mem2reg_simple_store_load() {
        let (mut ctx, mut module) = make_simple_fn();

        // Before: entry body = [alloca, store, load]
        assert_eq!(module.functions[0].blocks[0].body.len(), 3);

        let mut pass = Mem2Reg;
        let changed = pass.run_on_function(&mut ctx, &mut module.functions[0]);
        assert!(changed);

        // After: entry body is empty (all three promoted away).
        let func = &module.functions[0];
        assert_eq!(
            func.blocks[0].body.len(),
            0,
            "alloca, store, and load should all be removed"
        );

        // ret should use %x (ArgId 0) directly.
        let tid = func.blocks[0].terminator.unwrap();
        if let InstrKind::Ret { val: Some(v) } = &func.instr(tid).kind {
            assert_eq!(
                *v,
                ValueRef::Argument(llvm_ir::ArgId(0)),
                "ret should use arg %x directly after mem2reg"
            );
        } else {
            panic!("terminator should be ret with a value");
        }
    }

    // Build a function with an if-else that stores different values to an alloca,
    // then loads after the merge.  mem2reg should insert a phi at the merge block.
    //
    //   entry:  %p = alloca i32
    //           br i1 %cond, %then, %else
    //   then:   store i32 1, %p; br %merge
    //   else:   store i32 2, %p; br %merge
    //   merge:  %v = load i32, %p; ret %v
    fn make_phi_fn() -> (Context, Module) {
        let mut ctx = Context::new();
        let mut module = Module::new("test");
        let mut b = Builder::new(&mut ctx, &mut module);
        b.add_function(
            "f",
            b.ctx.i32_ty,
            vec![b.ctx.i1_ty],
            vec!["cond".into()],
            false,
            Linkage::External,
        );
        let entry = b.add_block("entry");
        let then_b = b.add_block("then");
        let else_b = b.add_block("else");
        let merge = b.add_block("merge");

        b.position_at_end(entry);
        let cond = b.get_arg(0);
        let p = b.build_alloca("p", b.ctx.i32_ty);
        b.build_cond_br(cond, then_b, else_b);

        b.position_at_end(then_b);
        let c1 = b.const_int(b.ctx.i32_ty, 1);
        b.build_store(c1, p);
        b.build_br(merge);

        b.position_at_end(else_b);
        let c2 = b.const_int(b.ctx.i32_ty, 2);
        b.build_store(c2, p);
        b.build_br(merge);

        b.position_at_end(merge);
        let v = b.build_load("v", b.ctx.i32_ty, p);
        b.build_ret(v);

        (ctx, module)
    }

    #[test]
    fn mem2reg_inserts_phi() {
        let (mut ctx, mut module) = make_phi_fn();
        let mut pass = Mem2Reg;
        let changed = pass.run_on_function(&mut ctx, &mut module.functions[0]);
        assert!(changed);

        let func = &module.functions[0];
        // merge block (BlockId 3) should have a phi as its first (and only body) instruction.
        let merge_body = &func.blocks[3].body;
        assert!(!merge_body.is_empty(), "merge block should have a phi");
        assert!(
            matches!(func.instr(merge_body[0]).kind, InstrKind::Phi { .. }),
            "first instruction in merge should be a phi"
        );
        // ret should use the phi result.
        let tid = func.blocks[3].terminator.unwrap();
        if let InstrKind::Ret {
            val: Some(ValueRef::Instruction(phi_iid)),
        } = &func.instr(tid).kind
        {
            assert!(
                matches!(func.instr(*phi_iid).kind, InstrKind::Phi { .. }),
                "ret should reference the inserted phi"
            );
        } else {
            panic!("ret should use the phi result");
        }
    }

    #[test]
    fn non_promotable_alloca_unchanged() {
        // Alloca whose address is passed to a call — not promotable.
        let mut ctx = Context::new();
        let mut module = Module::new("test");
        let mut b = Builder::new(&mut ctx, &mut module);
        // Declare an external function taking a pointer.
        let ptr_ty = b.ctx.ptr_ty;
        let void_ty = b.ctx.void_ty;
        let callee_ty = b.ctx.mk_fn_type(void_ty, vec![ptr_ty], false);
        b.add_function("f", b.ctx.i32_ty, vec![], vec![], false, Linkage::External);
        let entry = b.add_block("entry");
        b.position_at_end(entry);
        let p = b.build_alloca("p", b.ctx.i32_ty);
        // Call with p as argument — captures address.
        b.build_call(
            "",
            b.ctx.void_ty,
            callee_ty,
            ValueRef::Global(llvm_ir::GlobalId(0)),
            vec![p],
        );
        let c0 = b.const_int(b.ctx.i32_ty, 0);
        b.build_ret(c0);

        let before = module.functions[0].blocks[0].body.len();
        let mut pass = Mem2Reg;
        let changed = pass.run_on_function(&mut ctx, &mut module.functions[0]);
        assert!(!changed, "non-promotable alloca must not be removed");
        assert_eq!(module.functions[0].blocks[0].body.len(), before);
    }
}
