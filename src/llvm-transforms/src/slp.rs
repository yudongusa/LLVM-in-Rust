//! SLP (Superword Level Parallelism) auto-vectorization pass.
//!
//! Detects scalar chains of independent floating-point operations on
//! sequential memory addresses and packs them into vector IR operations:
//!
//! ```text
//! %l0 = load f64, ptr %p0       %vec = load <4 x f64>, ptr %p0
//! %l1 = load f64, ptr %p1   →   %r0  = extractelement %vec, 0
//! %l2 = load f64, ptr %p2       %r1  = extractelement %vec, 1
//! %l3 = load f64, ptr %p3       ...
//! %a0 = fadd %l0, %b0
//! %a1 = fadd %l1, %b1           %vadd = fadd <4 x f64> %vlhs, %vrhs
//! %a2 = fadd %l2, %b2           %a0 = extractelement %vadd, 0
//! %a3 = fadd %l3, %b3           ...
//! ```
//!
//! The resulting vector IR targets x86 ADDPD/MULPD (via the existing
//! `vector_fp_opcode` infrastructure in `llvm-target-x86`).

use crate::pass::FunctionPass;
use llvm_ir::{
    context::{BlockId, InstrId, TypeId, ValueRef},
    instruction::{FastMathFlags, InstrKind},
    types::TypeData,
    Context, Function,
};
use std::collections::{HashMap, HashSet};

// ---------------------------------------------------------------------------
// Public pass struct
// ---------------------------------------------------------------------------

/// SLP auto-vectorization pass.
///
/// Scans each basic block for independent groups of N scalar FP operations
/// (FAdd/FSub/FMul/FDiv) whose operands come from sequential memory loads
/// (i.e. GEP with base-pointer + consecutive integer indices).  Such groups
/// are fused into a single vector load + vector operation + N extractelement
/// instructions.
///
/// `min_width` is the minimum number of elements that must form a group
/// before vectorization is attempted (default 4).
#[derive(Debug, Clone)]
pub struct SlpVectorizer {
    /// Minimum group width (in scalar elements).  Default 4.
    pub min_width: usize,
}

impl Default for SlpVectorizer {
    fn default() -> Self {
        Self { min_width: 4 }
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Scalar FP opcode kind — used to ensure a group operates on a uniform op.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::enum_variant_names)]
enum FpOp {
    FAdd,
    FSub,
    FMul,
    FDiv,
}

/// A group of N scalar FP instructions that are candidates for SLP fusion.
struct SlpGroup {
    /// Instruction ids of the scalar ops, in sequential order.
    ops: Vec<InstrId>,
    /// The scalar element type (f32 or f64).
    elem_ty: TypeId,
    /// The uniform FP opcode.
    fp_op: FpOp,
    /// GEP base ValueRef shared by all lhs loads.
    lhs_base: ValueRef,
    /// GEP base ValueRef shared by all rhs loads.
    rhs_base: ValueRef,
    /// Starting GEP index for lhs (constant i64).
    lhs_start_idx: i64,
    /// Starting GEP index for rhs (constant i64).
    rhs_start_idx: i64,
}

// ---------------------------------------------------------------------------
// Pass implementation
// ---------------------------------------------------------------------------

impl FunctionPass for SlpVectorizer {
    fn name(&self) -> &'static str {
        "slp-vectorizer"
    }

    fn run_on_function(&mut self, ctx: &mut Context, func: &mut Function) -> bool {
        let mut changed = false;

        // Process each block independently.  We collect block ids first to
        // avoid borrowing issues while mutating the function.
        let block_ids: Vec<BlockId> = (0..func.blocks.len() as u32)
            .map(llvm_ir::context::BlockId)
            .collect();

        for bid in block_ids {
            changed |= self.process_block(ctx, func, bid);
        }

        changed
    }
}

impl SlpVectorizer {
    fn process_block(
        &mut self,
        ctx: &mut Context,
        func: &mut Function,
        bid: BlockId,
    ) -> bool {
        // Collect instruction ids in this block (body only, not terminator).
        let body: Vec<InstrId> = func.blocks[bid.0 as usize].body.clone();

        // Build a map: InstrId → index-in-body for ordering checks.
        let pos: HashMap<InstrId, usize> =
            body.iter().enumerate().map(|(i, &id)| (id, i)).collect();

        // Find all scalar FP ops in this block.
        let fp_instrs: Vec<InstrId> = body
            .iter()
            .copied()
            .filter(|&id| scalar_fp_op(&func.instructions[id.0 as usize].kind).is_some())
            .collect();

        if fp_instrs.len() < self.min_width {
            return false;
        }

        // Try to form groups of exactly `min_width` consecutive fp ops that:
        //   1. Share the same opcode
        //   2. Share the same element type
        //   3. Both lhs and rhs come from sequential GEP loads
        //   4. No op in the group depends on another in the group
        let mut groups: Vec<SlpGroup> = Vec::new();
        let mut used: HashSet<InstrId> = HashSet::new();

        'outer: for start in 0..fp_instrs.len() {
            let seed = fp_instrs[start];
            if used.contains(&seed) {
                continue;
            }

            let (seed_op, seed_lhs, seed_rhs, seed_ty) =
                match unpack_fp_instr_typed(func, ctx, seed) {
                    Some(x) => x,
                    None => continue,
                };

            // Check seed lhs and rhs are loads whose ptrs are GEPs.
            let (lhs_base_0, lhs_idx_0) =
                match load_gep_info(ctx, func, seed_lhs) {
                    Some(x) => x,
                    None => continue,
                };
            let (rhs_base_0, rhs_idx_0) =
                match load_gep_info(ctx, func, seed_rhs) {
                    Some(x) => x,
                    None => continue,
                };

            // Accumulate a group of `min_width` ops.
            let mut ops = vec![seed];

            let mut next_lhs_idx = lhs_idx_0 + 1;
            let mut next_rhs_idx = rhs_idx_0 + 1;

            for &candidate in &fp_instrs[start + 1..] {
                if ops.len() == self.min_width {
                    break;
                }
                if used.contains(&candidate) {
                    continue 'outer;
                }

                let (c_op, c_lhs, c_rhs, c_ty) =
                    match unpack_fp_instr_typed(func, ctx, candidate) {
                        Some(x) => x,
                        None => continue 'outer,
                    };

                if c_op != seed_op || c_ty != seed_ty {
                    continue 'outer;
                }

                // Verify sequential GEP loads.
                let (c_lhs_base, c_lhs_idx) = match load_gep_info(ctx, func, c_lhs) {
                    Some(x) => x,
                    None => continue 'outer,
                };
                let (c_rhs_base, c_rhs_idx) = match load_gep_info(ctx, func, c_rhs) {
                    Some(x) => x,
                    None => continue 'outer,
                };

                if c_lhs_base != lhs_base_0
                    || c_rhs_base != rhs_base_0
                    || c_lhs_idx != next_lhs_idx
                    || c_rhs_idx != next_rhs_idx
                {
                    continue 'outer;
                }

                // Independence check: candidate must not depend on any earlier
                // member of the group.
                let ops_set: HashSet<InstrId> = ops.iter().copied().collect();
                let cand_pos = pos[&candidate];
                for &prev_op in &ops {
                    if cand_pos < pos[&prev_op] {
                        continue 'outer;
                    }
                }
                // Verify candidate is not used by any previously collected op.
                // (Simple: we only need to check the group so far doesn't use
                // candidate's value, since all are in the same block.)
                for &prev_op_id in &ops {
                    for vref in func.instructions[prev_op_id.0 as usize].kind.operands() {
                        if let Some(iid) = instr_id_of(vref) {
                            if iid == candidate {
                                continue 'outer;
                            }
                        }
                    }
                }
                // Candidate must not use any op in the group.
                for vref in func.instructions[candidate.0 as usize].kind.operands() {
                    if let Some(iid) = instr_id_of(vref) {
                        if ops_set.contains(&iid) {
                            continue 'outer;
                        }
                    }
                }

                ops.push(candidate);
                next_lhs_idx += 1;
                next_rhs_idx += 1;
            }

            if ops.len() < self.min_width {
                continue;
            }

            // We have a valid group.
            for &id in &ops {
                used.insert(id);
            }
            groups.push(SlpGroup {
                ops,
                elem_ty: seed_ty,
                fp_op: seed_op,
                lhs_base: lhs_base_0,
                rhs_base: rhs_base_0,
                lhs_start_idx: lhs_idx_0,
                rhs_start_idx: rhs_idx_0,
            });
        }

        if groups.is_empty() {
            return false;
        }

        // Apply each group: insert vector instructions before the first scalar
        // op, then remove the scalar ops and loads.
        for group in groups {
            apply_group(ctx, func, bid, &group);
        }

        true
    }
}

// ---------------------------------------------------------------------------
// apply_group: materialize the vector replacement for one SlpGroup
// ---------------------------------------------------------------------------

fn apply_group(ctx: &mut Context, func: &mut Function, bid: BlockId, group: &SlpGroup) {
    let n = group.ops.len() as u32;
    let elem_ty = group.elem_ty;

    // Build the vector type <N x elem_ty>.
    let vec_ty = ctx.mk_vector(elem_ty, n, false);
    let ptr_ty = ctx.ptr_ty;
    let i32_ty = ctx.i32_ty;
    let i64_ty = ctx.i64_ty;

    // Helper: create a constant i64.
    let const_i64 = |ctx: &mut Context, v: i64| -> ValueRef {
        ValueRef::Constant(ctx.const_int(i64_ty, v as u64))
    };
    let const_i32 = |ctx: &mut Context, v: u32| -> ValueRef {
        ValueRef::Constant(ctx.const_int(i32_ty, v as u64))
    };

    // --- Determine insert position: just before the first scalar op ---
    let first_op_pos = func.blocks[bid.0 as usize]
        .body
        .iter()
        .position(|&id| id == group.ops[0])
        .expect("first op must be in block body");

    // --- Build lhs vector load ---
    // We need a GEP to the start of the lhs slice with element type = vec_ty.
    // Pattern: getelementptr elem_ty, ptr base, i64 lhs_start_idx
    // Then load <N x elem_ty>
    let lhs_gep_idx = const_i64(ctx, group.lhs_start_idx);
    let lhs_gep_kind = InstrKind::GetElementPtr {
        inbounds: true,
        base_ty: elem_ty,
        ptr: group.lhs_base,
        indices: vec![lhs_gep_idx],
    };
    let lhs_gep_id = alloc_and_insert(func, bid, first_op_pos, None, ptr_ty, lhs_gep_kind);

    let lhs_load_kind = InstrKind::Load {
        ty: vec_ty,
        ptr: ValueRef::Instruction(lhs_gep_id),
        align: None,
        volatile: false,
    };
    let lhs_load_id = alloc_and_insert(
        func,
        bid,
        first_op_pos + 1,
        Some("slp.lhs.vec".to_string()),
        vec_ty,
        lhs_load_kind,
    );

    // --- Build rhs vector load ---
    let same_base = group.lhs_base == group.rhs_base;
    let rhs_load_id = if same_base && group.lhs_start_idx == group.rhs_start_idx {
        // Same load, share it.
        lhs_load_id
    } else {
        let rhs_gep_idx = const_i64(ctx, group.rhs_start_idx);
        let rhs_gep_kind = InstrKind::GetElementPtr {
            inbounds: true,
            base_ty: elem_ty,
            ptr: group.rhs_base,
            indices: vec![rhs_gep_idx],
        };
        let insert_pos = func.blocks[bid.0 as usize]
            .body
            .iter()
            .position(|&id| id == group.ops[0])
            .unwrap()
            + 2;
        let rhs_gep_id =
            alloc_and_insert(func, bid, insert_pos, None, ptr_ty, rhs_gep_kind);

        let rhs_load_kind = InstrKind::Load {
            ty: vec_ty,
            ptr: ValueRef::Instruction(rhs_gep_id),
            align: None,
            volatile: false,
        };
        alloc_and_insert(
            func,
            bid,
            insert_pos + 1,
            Some("slp.rhs.vec".to_string()),
            vec_ty,
            rhs_load_kind,
        )
    };

    // --- Build vector FP op ---
    let vec_op_kind = match group.fp_op {
        FpOp::FAdd => InstrKind::FAdd {
            flags: FastMathFlags::default(),
            lhs: ValueRef::Instruction(lhs_load_id),
            rhs: ValueRef::Instruction(rhs_load_id),
        },
        FpOp::FSub => InstrKind::FSub {
            flags: FastMathFlags::default(),
            lhs: ValueRef::Instruction(lhs_load_id),
            rhs: ValueRef::Instruction(rhs_load_id),
        },
        FpOp::FMul => InstrKind::FMul {
            flags: FastMathFlags::default(),
            lhs: ValueRef::Instruction(lhs_load_id),
            rhs: ValueRef::Instruction(rhs_load_id),
        },
        FpOp::FDiv => InstrKind::FDiv {
            flags: FastMathFlags::default(),
            lhs: ValueRef::Instruction(lhs_load_id),
            rhs: ValueRef::Instruction(rhs_load_id),
        },
    };

    let op_insert_pos = func.blocks[bid.0 as usize]
        .body
        .iter()
        .position(|&id| id == group.ops[0])
        .unwrap();

    let vec_op_id = alloc_and_insert(
        func,
        bid,
        op_insert_pos,
        Some("slp.vec.op".to_string()),
        vec_ty,
        vec_op_kind,
    );

    // --- Build N extractelement instructions after the vec op ---
    // We replace each scalar op's uses with the corresponding extract.
    let mut extract_ids: Vec<InstrId> = Vec::with_capacity(n as usize);
    for lane in 0..n {
        let idx_val = const_i32(ctx, lane);
        let extract_kind = InstrKind::ExtractElement {
            vec: ValueRef::Instruction(vec_op_id),
            idx: idx_val,
        };
        let pos_after_op = func.blocks[bid.0 as usize]
            .body
            .iter()
            .position(|&id| id == vec_op_id)
            .unwrap()
            + 1
            + lane as usize;
        let eid = alloc_and_insert(
            func,
            bid,
            pos_after_op,
            Some(format!("slp.extract.{}", lane)),
            elem_ty,
            extract_kind,
        );
        extract_ids.push(eid);
    }

    // --- Rewrite uses of the old scalar ops throughout the block ---
    // Build a replacement map: old scalar op InstrId → new extract InstrId.
    let replacements: HashMap<InstrId, InstrId> =
        group.ops.iter().copied().zip(extract_ids.iter().copied()).collect();

    // Rewrite all instructions in the block.
    rewrite_uses_in_block(func, bid, &replacements);

    // --- Remove old scalar op instructions from the block body ---
    // (Scalar loads will be dead and removed by DCE.)
    let to_remove: HashSet<InstrId> = group.ops.iter().copied().collect();
    func.blocks[bid.0 as usize]
        .body
        .retain(|id| !to_remove.contains(id));
}

// ---------------------------------------------------------------------------
// Allocation and insertion helper
// ---------------------------------------------------------------------------

/// Allocate a new instruction in the function's flat pool and insert its id
/// into the block body at the given position.  Returns the new `InstrId`.
fn alloc_and_insert(
    func: &mut Function,
    bid: BlockId,
    pos: usize,
    name: Option<String>,
    ty: TypeId,
    kind: InstrKind,
) -> InstrId {
    use llvm_ir::instruction::Instruction;

    let id = InstrId(func.instructions.len() as u32);
    let instr = Instruction::new(name, ty, kind);
    func.instructions.push(instr);
    let body = &mut func.blocks[bid.0 as usize].body;
    let clamped_pos = pos.min(body.len());
    body.insert(clamped_pos, id);
    id
}

// ---------------------------------------------------------------------------
// Use rewriting
// ---------------------------------------------------------------------------

/// In all instructions of `bid`, replace any `ValueRef::Instruction(old_id)`
/// with `ValueRef::Instruction(new_id)` wherever old_id appears in the replacements map.
fn rewrite_uses_in_block(
    func: &mut Function,
    bid: BlockId,
    replacements: &HashMap<InstrId, InstrId>,
) {
    if replacements.is_empty() {
        return;
    }

    // Collect all instruction ids in the block (body + terminator).
    let all_ids: Vec<InstrId> = {
        let bb = &func.blocks[bid.0 as usize];
        let mut v: Vec<InstrId> = bb.body.clone();
        if let Some(t) = bb.terminator {
            v.push(t);
        }
        v
    };

    for iid in all_ids {
        // Skip instructions that are themselves the "new" extract ids — they
        // don't reference old ops.
        let new_vals: HashSet<InstrId> = replacements.values().copied().collect();
        if new_vals.contains(&iid) {
            continue;
        }

        let kind = func.instructions[iid.0 as usize].kind.clone();
        let new_kind = rewrite_kind(kind, replacements);
        func.instructions[iid.0 as usize].kind = new_kind;
    }
}

/// Walk all ValueRef fields in an InstrKind and apply replacements.
fn rewrite_kind(kind: InstrKind, rep: &HashMap<InstrId, InstrId>) -> InstrKind {
    fn rw(v: ValueRef, rep: &HashMap<InstrId, InstrId>) -> ValueRef {
        match v {
            ValueRef::Instruction(id) => {
                if let Some(&new_id) = rep.get(&id) {
                    ValueRef::Instruction(new_id)
                } else {
                    v
                }
            }
            other => other,
        }
    }

    match kind {
        InstrKind::FAdd { flags, lhs, rhs } => InstrKind::FAdd {
            flags,
            lhs: rw(lhs, rep),
            rhs: rw(rhs, rep),
        },
        InstrKind::FSub { flags, lhs, rhs } => InstrKind::FSub {
            flags,
            lhs: rw(lhs, rep),
            rhs: rw(rhs, rep),
        },
        InstrKind::FMul { flags, lhs, rhs } => InstrKind::FMul {
            flags,
            lhs: rw(lhs, rep),
            rhs: rw(rhs, rep),
        },
        InstrKind::FDiv { flags, lhs, rhs } => InstrKind::FDiv {
            flags,
            lhs: rw(lhs, rep),
            rhs: rw(rhs, rep),
        },
        InstrKind::Add { flags, lhs, rhs } => InstrKind::Add {
            flags,
            lhs: rw(lhs, rep),
            rhs: rw(rhs, rep),
        },
        InstrKind::Sub { flags, lhs, rhs } => InstrKind::Sub {
            flags,
            lhs: rw(lhs, rep),
            rhs: rw(rhs, rep),
        },
        InstrKind::Mul { flags, lhs, rhs } => InstrKind::Mul {
            flags,
            lhs: rw(lhs, rep),
            rhs: rw(rhs, rep),
        },
        InstrKind::Store { val, ptr, align, volatile } => InstrKind::Store {
            val: rw(val, rep),
            ptr: rw(ptr, rep),
            align,
            volatile,
        },
        InstrKind::Load { ty, ptr, align, volatile } => InstrKind::Load {
            ty,
            ptr: rw(ptr, rep),
            align,
            volatile,
        },
        InstrKind::Ret { val } => InstrKind::Ret {
            val: val.map(|v| rw(v, rep)),
        },
        InstrKind::Call {
            tail,
            callee_ty,
            callee,
            args,
        } => InstrKind::Call {
            tail,
            callee_ty,
            callee: rw(callee, rep),
            args: args.into_iter().map(|a| rw(a, rep)).collect(),
        },
        InstrKind::ICmp { pred, lhs, rhs } => InstrKind::ICmp {
            pred,
            lhs: rw(lhs, rep),
            rhs: rw(rhs, rep),
        },
        InstrKind::FCmp { flags, pred, lhs, rhs } => InstrKind::FCmp {
            flags,
            pred,
            lhs: rw(lhs, rep),
            rhs: rw(rhs, rep),
        },
        InstrKind::CondBr {
            cond,
            then_dest,
            else_dest,
        } => InstrKind::CondBr {
            cond: rw(cond, rep),
            then_dest,
            else_dest,
        },
        InstrKind::Select {
            cond,
            then_val,
            else_val,
        } => InstrKind::Select {
            cond: rw(cond, rep),
            then_val: rw(then_val, rep),
            else_val: rw(else_val, rep),
        },
        InstrKind::Phi { ty, incoming } => InstrKind::Phi {
            ty,
            incoming: incoming.into_iter().map(|(v, b)| (rw(v, rep), b)).collect(),
        },
        InstrKind::GetElementPtr {
            inbounds,
            base_ty,
            ptr,
            indices,
        } => InstrKind::GetElementPtr {
            inbounds,
            base_ty,
            ptr: rw(ptr, rep),
            indices: indices.into_iter().map(|v| rw(v, rep)).collect(),
        },
        InstrKind::ExtractElement { vec, idx } => InstrKind::ExtractElement {
            vec: rw(vec, rep),
            idx: rw(idx, rep),
        },
        InstrKind::InsertElement { vec, val, idx } => InstrKind::InsertElement {
            vec: rw(vec, rep),
            val: rw(val, rep),
            idx: rw(idx, rep),
        },
        // All other kinds: pass through unchanged (no FP scalar operands
        // we'd be replacing in practice).
        other => other,
    }
}

// ---------------------------------------------------------------------------
// Small query helpers
// ---------------------------------------------------------------------------

/// If `kind` is a scalar FP binary op, return `(FpOp, lhs, rhs)`.
fn scalar_fp_op(kind: &InstrKind) -> Option<(FpOp, ValueRef, ValueRef)> {
    match kind {
        InstrKind::FAdd { lhs, rhs, .. } => Some((FpOp::FAdd, *lhs, *rhs)),
        InstrKind::FSub { lhs, rhs, .. } => Some((FpOp::FSub, *lhs, *rhs)),
        InstrKind::FMul { lhs, rhs, .. } => Some((FpOp::FMul, *lhs, *rhs)),
        InstrKind::FDiv { lhs, rhs, .. } => Some((FpOp::FDiv, *lhs, *rhs)),
        _ => None,
    }
}

/// Resolve the element type for a FP instruction by looking at the instruction's
/// ty field.  Returns None if the type is a vector (already vectorized).
fn resolve_elem_ty(func: &Function, iid: InstrId, ctx: &Context) -> Option<TypeId> {
    let ty = func.instructions[iid.0 as usize].ty;
    match ctx.get_type(ty) {
        TypeData::Float(_) => Some(ty),
        _ => None,
    }
}

/// Full unpack: returns (FpOp, lhs, rhs, scalar_elem_ty) or None if the
/// instruction is not a scalar FP op.
fn unpack_fp_instr_typed(
    func: &Function,
    ctx: &Context,
    iid: InstrId,
) -> Option<(FpOp, ValueRef, ValueRef, TypeId)> {
    let instr = &func.instructions[iid.0 as usize];
    let elem_ty = resolve_elem_ty(func, iid, ctx)?;
    match &instr.kind {
        InstrKind::FAdd { flags: _, lhs, rhs } => Some((FpOp::FAdd, *lhs, *rhs, elem_ty)),
        InstrKind::FSub { flags: _, lhs, rhs } => Some((FpOp::FSub, *lhs, *rhs, elem_ty)),
        InstrKind::FMul { flags: _, lhs, rhs } => Some((FpOp::FMul, *lhs, *rhs, elem_ty)),
        InstrKind::FDiv { flags: _, lhs, rhs } => Some((FpOp::FDiv, *lhs, *rhs, elem_ty)),
        _ => None,
    }
}

/// If `vref` is an instruction that is a `load` whose pointer is a single-index
/// GEP over a constant integer index, return `(base_ptr, index_as_i64)`.
fn load_gep_info(ctx: &Context, func: &Function, vref: ValueRef) -> Option<(ValueRef, i64)> {
    let load_id = instr_id_of(vref)?;
    let load_instr = &func.instructions[load_id.0 as usize];
    let ptr_vref = match &load_instr.kind {
        InstrKind::Load { ptr, .. } => *ptr,
        _ => return None,
    };

    let gep_id = instr_id_of(ptr_vref)?;
    let gep_instr = &func.instructions[gep_id.0 as usize];
    match &gep_instr.kind {
        InstrKind::GetElementPtr { ptr, indices, .. } => {
            if indices.len() != 1 {
                return None;
            }
            let idx_val = indices[0];
            let idx_const = match idx_val {
                ValueRef::Constant(cid) => ctx.get_const(cid),
                _ => return None,
            };
            let idx_i64 = match idx_const {
                llvm_ir::value::ConstantData::Int { val, .. } => *val as i64,
                _ => return None,
            };
            Some((*ptr, idx_i64))
        }
        _ => None,
    }
}

/// Extract the `InstrId` from a `ValueRef::Instruction`.
fn instr_id_of(vref: ValueRef) -> Option<InstrId> {
    match vref {
        ValueRef::Instruction(id) => Some(id),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use llvm_ir::{Builder, Context, Linkage, Module};
    use llvm_ir::types::TypeData;

    /// Build a function that loads 4 f64 values at consecutive GEP offsets
    /// and fadds each pair. Verify that SLP produces a vector load + vector
    /// fadd + 4 extractelement instructions.
    ///
    /// IR shape (simplified):
    /// ```
    /// define void @f(ptr %a, ptr %b) {
    /// entry:
    ///   %a0  = getelementptr double, ptr %a, i64 0
    ///   %l0  = load double, ptr %a0
    ///   %a1  = getelementptr double, ptr %a, i64 1
    ///   %l1  = load double, ptr %a1
    ///   ...
    ///   %b0  = getelementptr double, ptr %b, i64 0
    ///   %lb0 = load double, ptr %b0
    ///   ...
    ///   %r0  = fadd double %l0, %lb0
    ///   %r1  = fadd double %l1, %lb1
    ///   %r2  = fadd double %l2, %lb2
    ///   %r3  = fadd double %l3, %lb3
    ///   ret void
    /// }
    /// ```
    /// Build a function that loads 4 f64 pairs, adds them, and stores results to `out`.
    /// The stores prevent DCE from eliminating the computation.
    fn build_f64x4_fadd_fn(ctx: &mut Context, module: &mut Module) {
        let mut b = Builder::new(ctx, module);
        let ptr_ty = b.ctx.ptr_ty;
        let f64_ty = b.ctx.f64_ty;
        let i64_ty = b.ctx.i64_ty;
        let void_ty = b.ctx.void_ty;

        b.add_function(
            "f",
            void_ty,
            vec![ptr_ty, ptr_ty, ptr_ty],
            vec!["a".into(), "b".into(), "out".into()],
            false,
            Linkage::External,
        );
        let entry = b.add_block("entry");
        b.position_at_end(entry);

        let a_ptr = b.get_arg(0);
        let b_ptr = b.get_arg(1);
        let out_ptr = b.get_arg(2);

        // Build 4 pairs of GEP+load from %a and %b.
        let mut a_loads = Vec::new();
        let mut b_loads_vec = Vec::new();
        for i in 0..4i64 {
            let idx = ValueRef::Constant(b.ctx.const_int(i64_ty, i as u64));
            let gep_a = b.build_gep_inbounds(format!("a{}", i), f64_ty, a_ptr, vec![idx]);
            let l_a = b.build_load(format!("la{}", i), f64_ty, gep_a);
            a_loads.push(l_a);

            let gep_b = b.build_gep_inbounds(format!("b{}", i), f64_ty, b_ptr, vec![idx]);
            let l_b = b.build_load(format!("lb{}", i), f64_ty, gep_b);
            b_loads_vec.push(l_b);
        }

        // Build 4 fadds and store results so DCE cannot eliminate the computation.
        for i in 0..4i64 {
            let r = b.build_fadd(format!("r{}", i), a_loads[i as usize], b_loads_vec[i as usize]);
            let idx = ValueRef::Constant(b.ctx.const_int(i64_ty, i as u64));
            let gep_out = b.build_gep_inbounds(format!("out{}", i), f64_ty, out_ptr, vec![idx]);
            b.build_store(r, gep_out);
        }

        b.build_ret_void();
    }

    #[test]
    fn slp_f64x4_fadd_vectorizes() {
        let mut ctx = Context::new();
        let mut module = Module::new("test");
        build_f64x4_fadd_fn(&mut ctx, &mut module);

        let mut pass = SlpVectorizer::default();
        let changed = pass.run_on_function(&mut ctx, &mut module.functions[0]);

        assert!(changed, "SLP should report a change");

        let func = &module.functions[0];
        // Check that the block body contains a vector FAdd.
        let has_vec_fadd = func.blocks[0].body.iter().any(|&id| {
            let instr = &func.instructions[id.0 as usize];
            match &instr.kind {
                InstrKind::FAdd { lhs, rhs, .. } => {
                    let lhs_ty = match lhs {
                        ValueRef::Instruction(iid) => func.instructions[iid.0 as usize].ty,
                        _ => return false,
                    };
                    matches!(ctx.get_type(lhs_ty), TypeData::Vector { .. })
                }
                _ => false,
            }
        });
        assert!(has_vec_fadd, "expected a vector FAdd instruction");

        // Check for exactly 4 extractelement instructions.
        let n_extracts = func.blocks[0]
            .body
            .iter()
            .filter(|&&id| {
                matches!(
                    &func.instructions[id.0 as usize].kind,
                    InstrKind::ExtractElement { .. }
                )
            })
            .count();
        assert_eq!(n_extracts, 4, "expected 4 extractelement instructions");

        // Check for a vector load.
        let has_vec_load = func.blocks[0].body.iter().any(|&id| {
            let instr = &func.instructions[id.0 as usize];
            match &instr.kind {
                InstrKind::Load { ty, .. } => {
                    matches!(ctx.get_type(*ty), TypeData::Vector { .. })
                }
                _ => false,
            }
        });
        assert!(has_vec_load, "expected a vector load");
    }

    /// Non-sequential accesses (stride 2) must NOT be vectorized.
    #[test]
    fn slp_non_sequential_not_vectorized() {
        let mut ctx = Context::new();
        let mut module = Module::new("test");

        let mut b = Builder::new(&mut ctx, &mut module);
        let ptr_ty = b.ctx.ptr_ty;
        let f64_ty = b.ctx.f64_ty;
        let i64_ty = b.ctx.i64_ty;
        let void_ty = b.ctx.void_ty;

        b.add_function(
            "g",
            void_ty,
            vec![ptr_ty, ptr_ty],
            vec!["a".into(), "b".into()],
            false,
            Linkage::External,
        );
        let entry = b.add_block("entry");
        b.position_at_end(entry);

        let a_ptr = b.get_arg(0);
        let b_ptr = b.get_arg(1);

        // Stride 2: indices [0, 2, 4, 6].
        let mut a_loads = Vec::new();
        let mut b_loads_v = Vec::new();
        for i in 0..4i64 {
            let idx_a = ValueRef::Constant(b.ctx.const_int(i64_ty, (i * 2) as u64));
            let idx_b = ValueRef::Constant(b.ctx.const_int(i64_ty, (i * 2) as u64));
            let gep_a = b.build_gep_inbounds(format!("a{}", i), f64_ty, a_ptr, vec![idx_a]);
            let l_a = b.build_load(format!("la{}", i), f64_ty, gep_a);
            a_loads.push(l_a);
            let gep_b = b.build_gep_inbounds(format!("b{}", i), f64_ty, b_ptr, vec![idx_b]);
            let l_b = b.build_load(format!("lb{}", i), f64_ty, gep_b);
            b_loads_v.push(l_b);
        }
        for i in 0..4 {
            b.build_fadd(format!("r{}", i), a_loads[i], b_loads_v[i]);
        }
        b.build_ret_void();

        let mut pass = SlpVectorizer::default();
        let changed = pass.run_on_function(&mut ctx, &mut module.functions[0]);
        assert!(!changed, "stride-2 accesses must not be vectorized");
    }

    /// Mixed element types (f32 and f64) must not be merged into a single group.
    #[test]
    fn slp_mixed_types_not_grouped() {
        let mut ctx = Context::new();
        let mut module = Module::new("test");

        let mut b = Builder::new(&mut ctx, &mut module);
        let ptr_ty = b.ctx.ptr_ty;
        let f32_ty = b.ctx.f32_ty;
        let f64_ty = b.ctx.f64_ty;
        let i64_ty = b.ctx.i64_ty;
        let void_ty = b.ctx.void_ty;

        b.add_function(
            "h",
            void_ty,
            vec![ptr_ty, ptr_ty, ptr_ty, ptr_ty],
            vec!["a32".into(), "b32".into(), "a64".into(), "b64".into()],
            false,
            Linkage::External,
        );
        let entry = b.add_block("entry");
        b.position_at_end(entry);

        let a32 = b.get_arg(0);
        let b32_arg = b.get_arg(1);
        let a64 = b.get_arg(2);
        let b64_arg = b.get_arg(3);

        // 2 f32 fadds (need min_width=4 to vectorize).
        for i in 0..2i64 {
            let idx = ValueRef::Constant(b.ctx.const_int(i64_ty, i as u64));
            let ga = b.build_gep_inbounds(format!("ga32_{}", i), f32_ty, a32, vec![idx]);
            let la = b.build_load(format!("la32_{}", i), f32_ty, ga);
            let gb = b.build_gep_inbounds(format!("gb32_{}", i), f32_ty, b32_arg, vec![idx]);
            let lb = b.build_load(format!("lb32_{}", i), f32_ty, gb);
            b.build_fadd(format!("r32_{}", i), la, lb);
        }
        // 2 f64 fadds (also < min_width=4).
        for i in 0..2i64 {
            let idx = ValueRef::Constant(b.ctx.const_int(i64_ty, i as u64));
            let ga = b.build_gep_inbounds(format!("ga64_{}", i), f64_ty, a64, vec![idx]);
            let la = b.build_load(format!("la64_{}", i), f64_ty, ga);
            let gb = b.build_gep_inbounds(format!("gb64_{}", i), f64_ty, b64_arg, vec![idx]);
            let lb = b.build_load(format!("lb64_{}", i), f64_ty, gb);
            b.build_fadd(format!("r64_{}", i), la, lb);
        }
        b.build_ret_void();

        let mut pass = SlpVectorizer::default();
        let changed = pass.run_on_function(&mut ctx, &mut module.functions[0]);
        // Total FP ops = 4, but they are split between f32 and f64, so no valid
        // group of 4 with uniform type exists.
        assert!(!changed, "mixed-type pairs below min_width must not vectorize");
    }

    /// O2 pipeline with sequential f64 array adds must produce vector IR.
    #[test]
    fn slp_in_o2_pipeline_produces_vector_ir() {
        use crate::pipeline::{build_pipeline, OptLevel};

        let mut ctx = Context::new();
        let mut module = Module::new("test");
        build_f64x4_fadd_fn(&mut ctx, &mut module);

        // SLP is wired into the O2 pipeline.
        let mut pm = build_pipeline(OptLevel::O2);
        pm.run_until_fixed_point(&mut ctx, &mut module, 8);

        let func = &module.functions[0];
        let has_vector_ty = func.instructions.iter().any(|instr| {
            matches!(ctx.get_type(instr.ty), TypeData::Vector { .. })
        });
        assert!(
            has_vector_ty,
            "after O2+SLP, function should contain at least one instruction with vector type"
        );
    }
}
