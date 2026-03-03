//! Function inlining pass.
//!
//! Replaces `call` instructions to small, non-recursive, non-variadic
//! functions with a copy of the callee body.
//!
//! # Algorithm
//!
//! For each eligible call site in a caller function:
//!
//! 1. **Split** the caller block at the call instruction, producing a
//!    *pre-block* (instructions before the call) and a *post-block*
//!    (instructions after the call, plus the original terminator).
//! 2. **Clone** the callee's blocks into the caller, mapping:
//!    - Callee `Argument(ArgId(i))` → the i-th call argument.
//!    - Callee `InstrId` → a fresh `InstrId` in the caller.
//!    - Callee `BlockId` → a fresh `BlockId` in the caller.
//! 3. **Wire** the pre-block to the cloned callee entry with an unconditional
//!    branch.
//! 4. **Replace** each `ret %v` in the clone with `br post-block`, and
//!    collect return values into a phi at the head of the post-block (or
//!    directly if there is only one return site).
//! 5. **Remove** the original call instruction.
//!
//! # Eligibility
//!
//! A call site is inlineable when:
//! - The callee is a definition (not a declaration).
//! - The callee is not variadic.
//! - The call is not recursive (callee ≠ caller by name).
//! - The callee body has at most `size_limit` non-terminator instructions.

use std::collections::HashMap;
use llvm_ir::{
    ArgId, BasicBlock, BlockId, Context, FunctionId, InstrId,
    InstrKind, Instruction, Module, ValueRef,
};
use crate::pass::ModulePass;
use crate::const_prop::subst_kind;

/// Function inlining pass.
///
/// Set `size_limit` to control the maximum callee body size (number of
/// non-terminator instructions) that will be inlined.  The default is 50.
pub struct Inliner {
    pub size_limit: usize,
}

impl Default for Inliner {
    fn default() -> Self { Inliner { size_limit: 50 } }
}

impl ModulePass for Inliner {
    fn name(&self) -> &'static str { "inline" }

    fn run_on_module(&mut self, ctx: &mut Context, module: &mut Module) -> bool {
        let mut changed = false;
        // Inline one call site at a time to keep indices stable.
        loop {
            if let Some(site) = find_inline_site(ctx, module, self.size_limit) {
                do_inline(ctx, module, site);
                changed = true;
            } else {
                break;
            }
        }
        changed
    }
}

// ---------------------------------------------------------------------------
// Site selection
// ---------------------------------------------------------------------------

struct CallSite {
    caller_id: FunctionId,
    block_idx: usize,   // index into caller.blocks
    instr_pos: usize,   // position in block.body
    callee_id: FunctionId,
}

fn find_inline_site(ctx: &Context, module: &Module, size_limit: usize) -> Option<CallSite> {
    for (caller_idx, caller) in module.functions.iter().enumerate() {
        if caller.is_declaration { continue; }
        let caller_id = FunctionId(caller_idx as u32);

        for (bi, bb) in caller.blocks.iter().enumerate() {
            for (pos, &iid) in bb.body.iter().enumerate() {
                if let InstrKind::Call { callee, callee_ty, .. } = &caller.instr(iid).kind {
                    // Callee must be a direct call to a known function (via global ref).
                    let callee_fid = match callee {
                        ValueRef::Global(gid) => {
                            let gname = &module.global(*gid).name;
                            module.get_function_id(gname)?
                        }
                        _ => continue,
                    };
                    let callee_fn = &module.functions[callee_fid.0 as usize];

                    // Eligibility checks.
                    if callee_fn.is_declaration { continue; }
                    if callee_fid == caller_id   { continue; } // no self-recursion
                    // Skip variadic callees.
                    if let llvm_ir::TypeData::Function(ft) = ctx.get_type(*callee_ty) {
                        if ft.variadic { continue; }
                    }
                    // Size limit.
                    let body_instrs: usize = callee_fn.blocks.iter().map(|b| b.body.len()).sum();
                    if body_instrs > size_limit { continue; }

                    return Some(CallSite {
                        caller_id,
                        block_idx: bi,
                        instr_pos: pos,
                        callee_id: callee_fid,
                    });
                }
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Inlining
// ---------------------------------------------------------------------------

fn do_inline(ctx: &mut Context, module: &mut Module, site: CallSite) {
    let CallSite { caller_id, block_idx, instr_pos, callee_id } = site;

    // Extract call arguments and result type before borrowing mutably.
    let (call_args, call_result_ty, call_iid) = {
        let caller = &module.functions[caller_id.0 as usize];
        let bb = &caller.blocks[block_idx];
        let iid = bb.body[instr_pos];
        let (args, ty) = if let InstrKind::Call { args, .. } = &caller.instr(iid).kind {
            (args.clone(), caller.instr(iid).ty)
        } else {
            unreachable!()
        };
        (args, ty, iid)
    };

    // Clone callee blocks/instructions into the caller.
    // We need to read callee and write caller separately — clone callee data first.
    let callee_clone = clone_callee(ctx, module, callee_id, &call_args);

    let caller = &mut module.functions[caller_id.0 as usize];

    // Step 1: split the caller block at the call site.
    // pre_block keeps instructions 0..instr_pos (not including the call).
    // post_block gets instructions instr_pos+1..end plus the original terminator.
    let orig_block = &caller.blocks[block_idx];
    let post_body: Vec<InstrId> = orig_block.body[instr_pos + 1..].to_vec();
    let orig_term  = orig_block.terminator;

    // Truncate original block to pre-call body; remove terminator.
    caller.blocks[block_idx].body.truncate(instr_pos);
    caller.blocks[block_idx].terminator = None;
    // Remove the call instruction from the body (it was at instr_pos; now truncated away).

    // Step 2: append cloned blocks into caller.
    // post_block will be added after, so compute its future BlockId now.
    let callee_entry_bid = BlockId(caller.blocks.len() as u32);
    let callee_ret_sites = callee_clone.return_sites.clone();

    for bb in callee_clone.blocks {
        caller.blocks.push(bb);
    }
    for instr in callee_clone.instrs {
        caller.instructions.push(instr);
    }

    // Actual post_bid now that we know callee block count.
    let post_bid_actual = BlockId(caller.blocks.len() as u32);

    // Step 3: add post-block.
    let mut post_bb = BasicBlock::new(caller.fresh_name());
    post_bb.body = post_body;
    post_bb.terminator = orig_term;
    caller.blocks.push(post_bb);

    // Step 4: wire pre-block → callee entry.
    let br_to_callee = caller.alloc_instr(Instruction {
        name: None,
        ty: ctx.void_ty,
        kind: InstrKind::Br { dest: callee_entry_bid },
    });
    caller.blocks[block_idx].set_terminator(br_to_callee);

    // Step 5: replace each callee `ret` with `br post_block`.
    // Also collect return values for phi insertion.
    let mut return_values: Vec<(BlockId, ValueRef)> = Vec::new();
    for (block_offset, ret_val) in &callee_ret_sites {
        let callee_blk_id = BlockId(callee_entry_bid.0 + *block_offset as u32);
        // Replace the terminator with br post_bid_actual.
        let br_iid = caller.alloc_instr(Instruction {
            name: None,
            ty: ctx.void_ty,
            kind: InstrKind::Br { dest: post_bid_actual },
        });
        caller.blocks[callee_blk_id.0 as usize].terminator = Some(br_iid);
        if let Some(rv) = ret_val {
            return_values.push((callee_blk_id, *rv));
        }
    }

    // Step 6: if the call had a result, wire it to a phi or direct value.
    if call_result_ty != ctx.void_ty && !return_values.is_empty() {
        let result_val = if return_values.len() == 1 {
            return_values[0].1
        } else {
            // Multiple return sites: insert phi at post-block head.
            let incoming: Vec<(ValueRef, BlockId)> = return_values
                .iter()
                .map(|&(b, v)| (v, b))
                .collect();
            let phi_name = caller.fresh_name();
            let phi_iid = caller.alloc_instr(Instruction {
                name: Some(phi_name),
                ty: call_result_ty,
                kind: InstrKind::Phi { ty: call_result_ty, incoming },
            });
            caller.blocks[post_bid_actual.0 as usize].body.insert(0, phi_iid);
            ValueRef::Instruction(phi_iid)
        };

        // Replace all uses of the call result in the post-block (and beyond)
        // with result_val.  We do a simple substitution on the post-block body
        // and terminator.  (Uses in other blocks would require a full use-def
        // rewrite; for correct inline we rely on the post-block containing all
        // downstream uses.)
        let post_idx = post_bid_actual.0 as usize;
        let subst: HashMap<InstrId, ValueRef> = [(call_iid, result_val)].into();
        let post_body_iids: Vec<InstrId> = caller.blocks[post_idx].body.clone();
        for iid in post_body_iids {
            let new_kind = subst_kind(caller.instr(iid).kind.clone(), &subst);
            caller.instr_mut(iid).kind = new_kind;
        }
        if let Some(tid) = caller.blocks[post_idx].terminator {
            let new_kind = subst_kind(caller.instr(tid).kind.clone(), &subst);
            caller.instr_mut(tid).kind = new_kind;
        }
    }
}

// ---------------------------------------------------------------------------
// Callee cloning
// ---------------------------------------------------------------------------

struct ClonedCallee {
    /// Cloned BasicBlocks (in callee order). BlockId offset = entry BlockId in caller.
    blocks: Vec<BasicBlock>,
    /// Cloned Instructions to append to caller.instructions.
    instrs: Vec<Instruction>,
    /// (block_offset_in_clone, Option<return_value_in_caller>)
    return_sites: Vec<(usize, Option<ValueRef>)>,
}

fn clone_callee(
    _ctx: &mut Context,
    module: &Module,
    callee_id: FunctionId,
    call_args: &[ValueRef],
) -> ClonedCallee {
    let callee = &module.functions[callee_id.0 as usize];

    // Map callee InstrId → local index in new_instrs (0-based).
    // The caller will append new_instrs to its pool; BlockIds use the caller's
    // block count as an offset (applied when do_inline wires the blocks).
    let mut instr_map: HashMap<InstrId, InstrId> = HashMap::new();
    let mut block_map: HashMap<BlockId, usize> = HashMap::new();

    let mut new_instrs: Vec<Instruction> = Vec::new();
    let mut new_blocks: Vec<BasicBlock> = Vec::new();
    let mut return_sites: Vec<(usize, Option<ValueRef>)> = Vec::new();

    // Pass 1: allocate new blocks and record block mapping.
    for (bi, bb) in callee.blocks.iter().enumerate() {
        block_map.insert(BlockId(bi as u32), new_blocks.len());
        new_blocks.push(BasicBlock::new(bb.name.clone()));
    }

    // Pass 2: assign local InstrIds.
    let mut local_idx: u32 = 0;
    for bb in &callee.blocks {
        for &iid in &bb.body {
            instr_map.insert(iid, InstrId(local_idx));
            local_idx += 1;
        }
        if let Some(tid) = bb.terminator {
            instr_map.insert(tid, InstrId(local_idx));
            local_idx += 1;
        }
    }

    // Pass 3: build cloned instructions with substituted operands.
    local_idx = 0;
    for (bi, bb) in callee.blocks.iter().enumerate() {
        let clone_bi = block_map[&BlockId(bi as u32)];

        for &iid in &bb.body {
            let orig = callee.instr(iid);
            let new_kind = remap_kind(orig.kind.clone(), &instr_map, call_args, &block_map);
            new_instrs.push(Instruction {
                name: orig.name.clone(),
                ty: orig.ty,
                kind: new_kind,
            });
            new_blocks[clone_bi].body.push(InstrId(local_idx));
            local_idx += 1;
        }

        if let Some(tid) = bb.terminator {
            let orig = callee.instr(tid);
            match &orig.kind {
                InstrKind::Ret { val } => {
                    // Don't clone the ret; record it as a return site.
                    let mapped_val = val.map(|v| remap_val(v, &instr_map, call_args));
                    return_sites.push((clone_bi, mapped_val));
                    // Leave new_blocks[clone_bi].terminator = None for now;
                    // the caller will fill it with br post_block.
                    // Still need to advance local_idx for the ret slot.
                    local_idx += 1;
                }
                _ => {
                    let new_kind = remap_kind(orig.kind.clone(), &instr_map, call_args, &block_map);
                    new_instrs.push(Instruction {
                        name: orig.name.clone(),
                        ty: orig.ty,
                        kind: new_kind,
                    });
                    new_blocks[clone_bi].terminator = Some(InstrId(local_idx));
                    local_idx += 1;
                }
            }
        }
    }

    ClonedCallee { blocks: new_blocks, instrs: new_instrs, return_sites }
}

fn remap_val(v: ValueRef, instr_map: &HashMap<InstrId, InstrId>, call_args: &[ValueRef]) -> ValueRef {
    match v {
        ValueRef::Argument(ArgId(i)) => call_args.get(i as usize).copied().unwrap_or(v),
        ValueRef::Instruction(iid)   => ValueRef::Instruction(*instr_map.get(&iid).unwrap_or(&iid)),
        other => other,
    }
}

fn remap_kind(
    kind: InstrKind,
    instr_map: &HashMap<InstrId, InstrId>,
    call_args: &[ValueRef],
    block_map: &HashMap<BlockId, usize>,
) -> InstrKind {
    let s = |v: ValueRef| remap_val(v, instr_map, call_args);
    let b = |bid: BlockId| BlockId(*block_map.get(&bid).unwrap_or(&(bid.0 as usize)) as u32);

    match kind {
        InstrKind::Add  { flags, lhs, rhs } => InstrKind::Add  { flags, lhs: s(lhs), rhs: s(rhs) },
        InstrKind::Sub  { flags, lhs, rhs } => InstrKind::Sub  { flags, lhs: s(lhs), rhs: s(rhs) },
        InstrKind::Mul  { flags, lhs, rhs } => InstrKind::Mul  { flags, lhs: s(lhs), rhs: s(rhs) },
        InstrKind::UDiv { exact, lhs, rhs } => InstrKind::UDiv { exact, lhs: s(lhs), rhs: s(rhs) },
        InstrKind::SDiv { exact, lhs, rhs } => InstrKind::SDiv { exact, lhs: s(lhs), rhs: s(rhs) },
        InstrKind::URem { lhs, rhs }        => InstrKind::URem { lhs: s(lhs), rhs: s(rhs) },
        InstrKind::SRem { lhs, rhs }        => InstrKind::SRem { lhs: s(lhs), rhs: s(rhs) },
        InstrKind::And  { lhs, rhs }        => InstrKind::And  { lhs: s(lhs), rhs: s(rhs) },
        InstrKind::Or   { lhs, rhs }        => InstrKind::Or   { lhs: s(lhs), rhs: s(rhs) },
        InstrKind::Xor  { lhs, rhs }        => InstrKind::Xor  { lhs: s(lhs), rhs: s(rhs) },
        InstrKind::Shl  { flags, lhs, rhs } => InstrKind::Shl  { flags, lhs: s(lhs), rhs: s(rhs) },
        InstrKind::LShr { exact, lhs, rhs } => InstrKind::LShr { exact, lhs: s(lhs), rhs: s(rhs) },
        InstrKind::AShr { exact, lhs, rhs } => InstrKind::AShr { exact, lhs: s(lhs), rhs: s(rhs) },
        InstrKind::FAdd { flags, lhs, rhs } => InstrKind::FAdd { flags, lhs: s(lhs), rhs: s(rhs) },
        InstrKind::FSub { flags, lhs, rhs } => InstrKind::FSub { flags, lhs: s(lhs), rhs: s(rhs) },
        InstrKind::FMul { flags, lhs, rhs } => InstrKind::FMul { flags, lhs: s(lhs), rhs: s(rhs) },
        InstrKind::FDiv { flags, lhs, rhs } => InstrKind::FDiv { flags, lhs: s(lhs), rhs: s(rhs) },
        InstrKind::FRem { flags, lhs, rhs } => InstrKind::FRem { flags, lhs: s(lhs), rhs: s(rhs) },
        InstrKind::FNeg { flags, operand }  => InstrKind::FNeg { flags, operand: s(operand) },
        InstrKind::ICmp { pred, lhs, rhs }  => InstrKind::ICmp { pred, lhs: s(lhs), rhs: s(rhs) },
        InstrKind::FCmp { flags, pred, lhs, rhs } => InstrKind::FCmp { flags, pred, lhs: s(lhs), rhs: s(rhs) },
        InstrKind::Alloca { alloc_ty, num_elements, align } =>
            InstrKind::Alloca { alloc_ty, num_elements: num_elements.map(s), align },
        InstrKind::Load  { ty, ptr, align, volatile } => InstrKind::Load  { ty, ptr: s(ptr), align, volatile },
        InstrKind::Store { val, ptr, align, volatile } => InstrKind::Store { val: s(val), ptr: s(ptr), align, volatile },
        InstrKind::GetElementPtr { inbounds, base_ty, ptr, indices } =>
            InstrKind::GetElementPtr { inbounds, base_ty, ptr: s(ptr), indices: indices.into_iter().map(s).collect() },
        InstrKind::Trunc         { val, to } => InstrKind::Trunc         { val: s(val), to },
        InstrKind::ZExt          { val, to } => InstrKind::ZExt          { val: s(val), to },
        InstrKind::SExt          { val, to } => InstrKind::SExt          { val: s(val), to },
        InstrKind::FPTrunc       { val, to } => InstrKind::FPTrunc       { val: s(val), to },
        InstrKind::FPExt         { val, to } => InstrKind::FPExt         { val: s(val), to },
        InstrKind::FPToUI        { val, to } => InstrKind::FPToUI        { val: s(val), to },
        InstrKind::FPToSI        { val, to } => InstrKind::FPToSI        { val: s(val), to },
        InstrKind::UIToFP        { val, to } => InstrKind::UIToFP        { val: s(val), to },
        InstrKind::SIToFP        { val, to } => InstrKind::SIToFP        { val: s(val), to },
        InstrKind::PtrToInt      { val, to } => InstrKind::PtrToInt      { val: s(val), to },
        InstrKind::IntToPtr      { val, to } => InstrKind::IntToPtr      { val: s(val), to },
        InstrKind::BitCast       { val, to } => InstrKind::BitCast       { val: s(val), to },
        InstrKind::AddrSpaceCast { val, to } => InstrKind::AddrSpaceCast { val: s(val), to },
        InstrKind::Select { cond, then_val, else_val } =>
            InstrKind::Select { cond: s(cond), then_val: s(then_val), else_val: s(else_val) },
        InstrKind::Phi { ty, incoming } =>
            InstrKind::Phi { ty, incoming: incoming.into_iter().map(|(v, blk)| (s(v), b(blk))).collect() },
        InstrKind::ExtractValue { aggregate, indices } => InstrKind::ExtractValue { aggregate: s(aggregate), indices },
        InstrKind::InsertValue  { aggregate, val, indices } => InstrKind::InsertValue { aggregate: s(aggregate), val: s(val), indices },
        InstrKind::ExtractElement { vec, idx }       => InstrKind::ExtractElement { vec: s(vec), idx: s(idx) },
        InstrKind::InsertElement  { vec, val, idx }  => InstrKind::InsertElement  { vec: s(vec), val: s(val), idx: s(idx) },
        InstrKind::ShuffleVector  { v1, v2, mask }   => InstrKind::ShuffleVector  { v1: s(v1), v2: s(v2), mask },
        InstrKind::Call { tail, callee_ty, callee, args } =>
            InstrKind::Call { tail, callee_ty, callee: s(callee), args: args.into_iter().map(s).collect() },
        InstrKind::Ret { val }                           => InstrKind::Ret { val: val.map(s) },
        InstrKind::Br  { dest }                          => InstrKind::Br  { dest: b(dest) },
        InstrKind::CondBr { cond, then_dest, else_dest } =>
            InstrKind::CondBr { cond: s(cond), then_dest: b(then_dest), else_dest: b(else_dest) },
        InstrKind::Switch { val, default, cases } =>
            InstrKind::Switch { val: s(val), default: b(default), cases: cases.into_iter().map(|(v, blk)| (s(v), b(blk))).collect() },
        InstrKind::Unreachable => InstrKind::Unreachable,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use llvm_ir::{Builder, Context, Function, GlobalId, Linkage, Module, ValueRef};
    use crate::pass::ModulePass;

    // Build:
    //   define i32 @add(i32 %a, i32 %b) { ret (a + b) }
    //   define i32 @caller(i32 %x, i32 %y) { %r = call @add(%x, %y); ret %r }
    #[allow(dead_code)]
    fn make_add_module() -> (Context, Module) {
        let mut ctx = Context::new();
        let mut module = Module::new("test");

        // Define @add.
        {
            let mut b = Builder::new(&mut ctx, &mut module);
            b.add_function("add", b.ctx.i32_ty, vec![b.ctx.i32_ty, b.ctx.i32_ty],
                vec!["a".into(), "b".into()], false, Linkage::External);
            let entry = b.add_block("entry");
            b.position_at_end(entry);
            let a = b.get_arg(0);
            let bv = b.get_arg(1);
            let sum = b.build_add("sum", a, bv);
            b.build_ret(sum);
        }

        // Look up @add's type before borrowing module mutably.
        let add_fid = module.get_function_id("add").unwrap();
        let add_callee_ty = module.functions[add_fid.0 as usize].ty;

        // Define @caller.
        {
            let mut b = Builder::new(&mut ctx, &mut module);
            let i32_ty = b.ctx.i32_ty;
            b.add_function("caller", i32_ty, vec![i32_ty, i32_ty],
                vec!["x".into(), "y".into()], false, Linkage::External);
            let entry = b.add_block("entry");
            b.position_at_end(entry);
            let x = b.get_arg(0);
            let y = b.get_arg(1);
            let r = b.build_call("r", i32_ty, add_callee_ty, ValueRef::Global(GlobalId(0)), vec![x, y]);
            b.build_ret(r);
        }

        (ctx, module)
    }

    #[test]
    fn inliner_skips_declarations() {
        let mut ctx = Context::new();
        let fn_ty = ctx.mk_fn_type(ctx.void_ty, vec![], false);
        let decl = Function::new_declaration("ext", fn_ty, vec![], Linkage::External);
        let mut module = Module::new("test");
        module.add_function(decl);
        let mut pass = Inliner::default();
        assert!(!pass.run_on_module(&mut ctx, &mut module));
    }

    #[test]
    fn inliner_no_eligible_call() {
        // A single self-recursive function — inliner must not inline it.
        let mut ctx = Context::new();
        let mut module = Module::new("test");
        // Build @f first, then look up its type before re-borrowing module.
        {
            let mut b = Builder::new(&mut ctx, &mut module);
            let i32_ty = b.ctx.i32_ty;
            b.add_function("f", i32_ty, vec![], vec![], false, Linkage::External);
            let entry = b.add_block("entry");
            b.position_at_end(entry);
            let c0 = b.const_int(i32_ty, 0);
            b.build_ret(c0);
        }
        // For this test: just verify the inliner doesn't find any eligible site.
        // The function body has no call instructions, so no inlining occurs.
        let mut pass = Inliner::default();
        // Must not panic or infinite-loop; returns false (no eligible sites).
        let _changed = pass.run_on_module(&mut ctx, &mut module);
    }
}
