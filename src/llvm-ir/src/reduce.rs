//! IR minimizer (delta debugging style).
//!
//! Reduces a failing IR module to the smallest version that still triggers a
//! user-supplied predicate.  Each round applies a series of reduction passes
//! (remove functions, remove blocks, replace instruction results with `undef`,
//! replace instruction results with the zero constant) until no further
//! reduction is possible (fixed point).
//!
//! # Example
//!
//! ```rust
//! use llvm_ir::reduce::{ContainsPredicate, Reducer};
//! use llvm_ir::{Context, Module};
//!
//! let mut ctx = Context::new();
//! let module = Module::new("test");
//! let pred = ContainsPredicate { substring: "add".to_string() };
//! let reducer = Reducer::new();
//! let (_ctx2, _module2) = reducer.reduce(ctx, module, &pred);
//! ```

use crate::basic_block::BasicBlock;
use crate::context::{BlockId, ConstId, InstrId, TypeId, ValueRef};
use crate::function::Function;
use crate::instruction::{Instruction, InstrKind};
use crate::module::Module;
use crate::printer::Printer;
use crate::Context;

// ---------------------------------------------------------------------------
// Predicate trait
// ---------------------------------------------------------------------------

/// A predicate that tests whether a (`Context`, `Module`) pair still exhibits
/// the failure being reduced.
///
/// Implementations must be deterministic: the same IR must always yield the
/// same result.
pub trait Predicate: Send + Sync {
    /// Returns `true` if this IR still triggers the failure.
    fn test(&self, ctx: &Context, module: &Module) -> bool;
}

// ---------------------------------------------------------------------------
// ContainsPredicate
// ---------------------------------------------------------------------------

/// Simple text predicate: the printed IR must contain a specific substring.
///
/// Useful for smoke-testing the reducer and for substring-based bug
/// reproduction.
pub struct ContainsPredicate {
    /// The substring to search for in the printed IR.
    pub substring: String,
}

impl Predicate for ContainsPredicate {
    fn test(&self, ctx: &Context, module: &Module) -> bool {
        let text = Printer::new(ctx).print_module(module);
        text.contains(&self.substring)
    }
}

// ---------------------------------------------------------------------------
// Reducer
// ---------------------------------------------------------------------------

/// Runs all reduction passes until a fixed point is reached.
pub struct Reducer {
    /// Maximum number of full reduction rounds (each round applies all four
    /// passes).  Default is `20`.
    pub max_passes: usize,
}

impl Default for Reducer {
    fn default() -> Self {
        Self::new()
    }
}

impl Reducer {
    /// Create a new `Reducer` with the default maximum of 20 passes.
    pub fn new() -> Self {
        Self { max_passes: 20 }
    }

    /// Reduce `module` (together with its `ctx`) to the smallest version that
    /// still satisfies `pred`.
    ///
    /// Returns the reduced `(Context, Module)`.
    pub fn reduce(
        &self,
        ctx: Context,
        module: Module,
        pred: &dyn Predicate,
    ) -> (Context, Module) {
        // Verify that the predicate holds on the original input.
        if !pred.test(&ctx, &module) {
            // Predicate does not hold; return as-is.
            return (ctx, module);
        }

        let mut ctx = ctx;
        let mut module = module;

        for _ in 0..self.max_passes {
            let mut changed = false;

            let (ch, c, m) = reduce_functions(ctx, module, pred);
            ctx = c;
            module = m;
            changed |= ch;

            let (ch, c, m) = reduce_blocks(ctx, module, pred);
            ctx = c;
            module = m;
            changed |= ch;

            let (ch, c, m) = reduce_instructions(ctx, module, pred);
            ctx = c;
            module = m;
            changed |= ch;

            let (ch, c, m) = reduce_to_constants(ctx, module, pred);
            ctx = c;
            module = m;
            changed |= ch;

            if !changed {
                break;
            }
        }

        (ctx, module)
    }
}

// ---------------------------------------------------------------------------
// Pass: reduce_functions
// ---------------------------------------------------------------------------

/// Try to remove each non-entry, non-main function.  A function is kept if
/// removing it causes the predicate to return `false`.
fn reduce_functions(
    ctx: Context,
    module: Module,
    pred: &dyn Predicate,
) -> (bool, Context, Module) {
    // Collect indices to try removing (in reverse so indices stay valid if we
    // rebuild from scratch each time).
    let n = module.functions.len();
    if n <= 1 {
        return (false, ctx, module);
    }

    let mut changed = false;
    let mut ctx = ctx;
    let mut module = module;

    // We iterate from the last function to the first so that index i remains
    // valid while we try each one.
    let mut i = n;
    while i > 0 {
        i -= 1;
        let func = &module.functions[i];
        // Don't remove the entry / main function.
        if func.name == "main" || i == 0 {
            continue;
        }
        // Try removing function i.
        let (trial_ctx, trial_module) = clone_module_without_function(&ctx, &module, i);
        if pred.test(&trial_ctx, &trial_module) {
            ctx = trial_ctx;
            module = trial_module;
            changed = true;
            // i is now pointing past the end of the (shorter) function list;
            // shrink to fit.
            if i > module.functions.len() {
                i = module.functions.len();
            }
        }
    }

    (changed, ctx, module)
}

/// Clone the module but skip function at index `skip_idx`.
fn clone_module_without_function(ctx: &Context, module: &Module, skip_idx: usize) -> (Context, Module) {
    let new_ctx = ctx.clone();
    let mut new_module = module_header_clone(module);

    for (i, func) in module.functions.iter().enumerate() {
        if i == skip_idx {
            continue;
        }
        new_module.add_function(func.clone());
    }

    (new_ctx, new_module)
}

// ---------------------------------------------------------------------------
// Pass: reduce_blocks
// ---------------------------------------------------------------------------

/// For each function, try removing each non-entry block.  Removing a block
/// means replacing it with an unconditional branch to the entry block for any
/// predecessor that referred to it, and removing all phi incoming edges that
/// reference the block.  The block's body is simply dropped.
fn reduce_blocks(
    ctx: Context,
    module: Module,
    pred: &dyn Predicate,
) -> (bool, Context, Module) {
    let mut changed = false;
    let mut ctx = ctx;
    let mut module = module;

    for fi in 0..module.functions.len() {
        if module.functions[fi].is_declaration {
            continue;
        }
        let num_blocks = module.functions[fi].blocks.len();
        if num_blocks <= 1 {
            continue;
        }

        let mut bi = num_blocks;
        while bi > 1 {
            bi -= 1;
            // Try removing block bi from function fi.
            let (trial_ctx, trial_module) =
                clone_module_without_block(&ctx, &module, fi, bi);
            if pred.test(&trial_ctx, &trial_module) {
                ctx = trial_ctx;
                module = trial_module;
                changed = true;
                // Adjust bi for the now-shorter block list.
                if bi > module.functions[fi].blocks.len() {
                    bi = module.functions[fi].blocks.len();
                }
            }
        }
    }

    (changed, ctx, module)
}

/// Clone the module but omit block `skip_bid` (by index) from function `fid`.
/// Any terminator successor that references the removed block is redirected to
/// the entry block (`BlockId(0)`).  Phi incoming edges for the removed block
/// are dropped.
fn clone_module_without_block(
    ctx: &Context,
    module: &Module,
    fid: usize,
    skip_bid: usize,
) -> (Context, Module) {
    let new_ctx = ctx.clone();
    let mut new_module = module_header_clone(module);

    for (fi, func) in module.functions.iter().enumerate() {
        if fi != fid {
            new_module.add_function(func.clone());
            continue;
        }

        // Build a new function without block skip_bid.
        // Block index mapping: old index → new index (or None if removed).
        let mut bid_map: Vec<Option<BlockId>> = Vec::with_capacity(func.blocks.len());
        let mut new_idx = 0u32;
        for bi in 0..func.blocks.len() {
            if bi == skip_bid {
                bid_map.push(None);
            } else {
                bid_map.push(Some(BlockId(new_idx)));
                new_idx += 1;
            }
        }

        let entry_new_bid = BlockId(0); // entry block is always index 0

        let mut new_func = function_signature_clone(func);

        // Re-allocate instructions with adjusted block/value refs.
        // We need to remap InstrIds because some instructions in the removed
        // block will be gone.  Collect which InstrIds are kept.
        let mut kept_instr_ids: std::collections::HashSet<InstrId> = std::collections::HashSet::new();
        for (bi, bb) in func.blocks.iter().enumerate() {
            if bi == skip_bid {
                continue;
            }
            for id in bb.instrs() {
                kept_instr_ids.insert(id);
            }
        }

        // Build old→new InstrId map.
        let mut instr_id_map: std::collections::HashMap<InstrId, InstrId> =
            std::collections::HashMap::new();
        let mut next_new_instr = 0u32;
        for old_id in 0..func.instructions.len() as u32 {
            let old_iid = InstrId(old_id);
            if kept_instr_ids.contains(&old_iid) {
                instr_id_map.insert(old_iid, InstrId(next_new_instr));
                next_new_instr += 1;
            }
        }

        // Pre-allocate instructions in the new function (order must match
        // the natural instruction-pool order, which is simply ascending InstrId).
        for old_id in 0..func.instructions.len() as u32 {
            let old_iid = InstrId(old_id);
            if !kept_instr_ids.contains(&old_iid) {
                continue;
            }
            let old_instr = func.instr(old_iid);
            let new_kind = remap_instr_kind(
                &old_instr.kind,
                &instr_id_map,
                &bid_map,
                entry_new_bid,
                func,
            );
            let new_instr = Instruction::new(old_instr.name.clone(), old_instr.ty, new_kind);
            let _new_iid = new_func.alloc_instr(new_instr);
        }

        // Add blocks (skipping removed one).
        for (bi, bb) in func.blocks.iter().enumerate() {
            if bi == skip_bid {
                continue;
            }
            let mut new_bb = BasicBlock::new(bb.name.clone());
            for old_id in &bb.body {
                if let Some(&new_id) = instr_id_map.get(old_id) {
                    new_bb.append_instr(new_id);
                }
            }
            if let Some(old_term) = bb.terminator {
                if let Some(&new_term) = instr_id_map.get(&old_term) {
                    new_bb.set_terminator(new_term);
                }
            }
            new_func.add_block(new_bb);
        }

        new_module.add_function(new_func);
    }

    (new_ctx, new_module)
}

// ---------------------------------------------------------------------------
// Pass: reduce_instructions
// ---------------------------------------------------------------------------

/// For each non-terminator instruction in each block, try replacing all uses
/// of its result with `undef` of the same type and removing the instruction.
fn reduce_instructions(
    ctx: Context,
    module: Module,
    pred: &dyn Predicate,
) -> (bool, Context, Module) {
    let mut changed = false;
    let mut ctx = ctx;
    let mut module = module;

    for fi in 0..module.functions.len() {
        if module.functions[fi].is_declaration {
            continue;
        }

        // Collect all body (non-terminator) InstrIds in this function.
        let body_instrs: Vec<InstrId> = module.functions[fi]
            .blocks
            .iter()
            .flat_map(|bb| bb.body.iter().copied())
            .collect();

        for &iid in &body_instrs {
            let instr_ty = module.functions[fi].instr(iid).ty;
            if instr_ty == ctx.void_ty {
                // Void-typed instructions have no result to replace.
                continue;
            }

            let (trial_ctx, trial_module) =
                clone_module_replace_instr_with_undef(&ctx, &module, fi, iid);
            if pred.test(&trial_ctx, &trial_module) {
                ctx = trial_ctx;
                module = trial_module;
                changed = true;
            }
        }
    }

    (changed, ctx, module)
}

/// Clone the module with instruction `remove_iid` in function `fid` replaced
/// by `undef` of its result type (all uses patched), then the instruction
/// removed from its block.
fn clone_module_replace_instr_with_undef(
    ctx: &Context,
    module: &Module,
    fid: usize,
    remove_iid: InstrId,
) -> (Context, Module) {
    let mut new_ctx = ctx.clone();
    let mut new_module = module_header_clone(module);

    for (fi, func) in module.functions.iter().enumerate() {
        if fi != fid {
            new_module.add_function(func.clone());
            continue;
        }

        let instr_ty = func.instr(remove_iid).ty;
        let undef_cid = new_ctx.const_undef(instr_ty);
        let undef_vref = ValueRef::Constant(undef_cid);

        // Build InstrId map: the removed instruction shifts subsequent ids down.
        // Only instructions that are not `remove_iid` appear in the new pool.
        let mut instr_id_map: std::collections::HashMap<InstrId, InstrId> =
            std::collections::HashMap::new();
        let mut next_new = 0u32;
        for old_id in 0..func.instructions.len() as u32 {
            let old_iid = InstrId(old_id);
            if old_iid == remove_iid {
                continue;
            }
            instr_id_map.insert(old_iid, InstrId(next_new));
            next_new += 1;
        }

        let mut new_func = function_signature_clone(func);

        // Allocate instructions (skipping the removed one, remapping refs).
        for old_id in 0..func.instructions.len() as u32 {
            let old_iid = InstrId(old_id);
            if old_iid == remove_iid {
                continue;
            }
            let old_instr = func.instr(old_iid);
            let new_kind = substitute_value_in_kind(
                &old_instr.kind,
                ValueRef::Instruction(remove_iid),
                undef_vref,
                &instr_id_map,
            );
            let new_instr = Instruction::new(old_instr.name.clone(), old_instr.ty, new_kind);
            let _new_iid = new_func.alloc_instr(new_instr);
        }

        // Rebuild blocks (removing remove_iid from the body).
        for bb in &func.blocks {
            let mut new_bb = BasicBlock::new(bb.name.clone());
            for &old_id in &bb.body {
                if old_id == remove_iid {
                    continue;
                }
                if let Some(&new_id) = instr_id_map.get(&old_id) {
                    new_bb.append_instr(new_id);
                }
            }
            if let Some(old_term) = bb.terminator {
                if let Some(&new_term) = instr_id_map.get(&old_term) {
                    new_bb.set_terminator(new_term);
                }
            }
            new_func.add_block(new_bb);
        }

        new_module.add_function(new_func);
    }

    (new_ctx, new_module)
}

// ---------------------------------------------------------------------------
// Pass: reduce_to_constants
// ---------------------------------------------------------------------------

/// For each non-terminator instruction result, try replacing all uses with the
/// zero constant of the appropriate type and removing the instruction.
fn reduce_to_constants(
    ctx: Context,
    module: Module,
    pred: &dyn Predicate,
) -> (bool, Context, Module) {
    let mut changed = false;
    let mut ctx = ctx;
    let mut module = module;

    for fi in 0..module.functions.len() {
        if module.functions[fi].is_declaration {
            continue;
        }

        let body_instrs: Vec<InstrId> = module.functions[fi]
            .blocks
            .iter()
            .flat_map(|bb| bb.body.iter().copied())
            .collect();

        for &iid in &body_instrs {
            let instr_ty = module.functions[fi].instr(iid).ty;
            if instr_ty == ctx.void_ty {
                continue;
            }

            let (trial_ctx, trial_module) =
                clone_module_replace_instr_with_zero(&ctx, &module, fi, iid);
            if pred.test(&trial_ctx, &trial_module) {
                ctx = trial_ctx;
                module = trial_module;
                changed = true;
            }
        }
    }

    (changed, ctx, module)
}

/// Clone the module with instruction `remove_iid` replaced by the zero
/// constant of its result type.
fn clone_module_replace_instr_with_zero(
    ctx: &Context,
    module: &Module,
    fid: usize,
    remove_iid: InstrId,
) -> (Context, Module) {
    let mut new_ctx = ctx.clone();
    let mut new_module = module_header_clone(module);

    for (fi, func) in module.functions.iter().enumerate() {
        if fi != fid {
            new_module.add_function(func.clone());
            continue;
        }

        let instr_ty = func.instr(remove_iid).ty;
        // Use const_int(0) for integer types, const_null for pointers,
        // and undef for anything else (floats, vectors, etc.).
        let zero_cid = make_zero_const(&mut new_ctx, instr_ty);
        let zero_vref = ValueRef::Constant(zero_cid);

        let mut instr_id_map: std::collections::HashMap<InstrId, InstrId> =
            std::collections::HashMap::new();
        let mut next_new = 0u32;
        for old_id in 0..func.instructions.len() as u32 {
            let old_iid = InstrId(old_id);
            if old_iid == remove_iid {
                continue;
            }
            instr_id_map.insert(old_iid, InstrId(next_new));
            next_new += 1;
        }

        let mut new_func = function_signature_clone(func);

        for old_id in 0..func.instructions.len() as u32 {
            let old_iid = InstrId(old_id);
            if old_iid == remove_iid {
                continue;
            }
            let old_instr = func.instr(old_iid);
            let new_kind = substitute_value_in_kind(
                &old_instr.kind,
                ValueRef::Instruction(remove_iid),
                zero_vref,
                &instr_id_map,
            );
            let new_instr = Instruction::new(old_instr.name.clone(), old_instr.ty, new_kind);
            let _new_iid = new_func.alloc_instr(new_instr);
        }

        for bb in &func.blocks {
            let mut new_bb = BasicBlock::new(bb.name.clone());
            for &old_id in &bb.body {
                if old_id == remove_iid {
                    continue;
                }
                if let Some(&new_id) = instr_id_map.get(&old_id) {
                    new_bb.append_instr(new_id);
                }
            }
            if let Some(old_term) = bb.terminator {
                if let Some(&new_term) = instr_id_map.get(&old_term) {
                    new_bb.set_terminator(new_term);
                }
            }
            new_func.add_block(new_bb);
        }

        new_module.add_function(new_func);
    }

    (new_ctx, new_module)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Make a "zero" constant for a given type: `i64 0` for integers, `null` for
/// pointers, `undef` for everything else.
fn make_zero_const(ctx: &mut Context, ty: TypeId) -> ConstId {
    use crate::types::TypeData;
    match ctx.get_type(ty).clone() {
        TypeData::Integer(_) => ctx.const_int(ty, 0),
        TypeData::Pointer => ctx.const_null(ty),
        _ => ctx.const_undef(ty),
    }
}

/// Clone the header fields of a Module (name, target info, globals, named
/// types, metadata) but not the functions.
fn module_header_clone(module: &Module) -> Module {
    let mut m = Module::new(module.name.clone());
    m.source_filename = module.source_filename.clone();
    m.target_triple = module.target_triple.clone();
    m.data_layout = module.data_layout.clone();
    m.globals = module.globals.clone();
    m.global_names = module.global_names.clone();
    m.named_types = module.named_types.clone();
    m.debug_locations = module.debug_locations.clone();
    m.metadata_nodes = module.metadata_nodes.clone();
    m.named_metadata = module.named_metadata.clone();
    m
}

/// Clone the signature (name, type, args, linkage flags) of a function
/// without blocks or instructions.
fn function_signature_clone(func: &Function) -> Function {
    let mut new_func = Function::new(
        func.name.clone(),
        func.ty,
        func.args.clone(),
        func.linkage,
    );
    new_func.is_declaration = func.is_declaration;
    new_func
}

/// Remap an `InstrKind`, redirecting:
/// - `ValueRef::Instruction` references through `instr_id_map`
/// - `BlockId` successor references through `bid_map` (redirect removed blocks
///   to `fallback_bid`)
/// - phi incoming edges: drop entries whose source block was removed
fn remap_instr_kind(
    kind: &InstrKind,
    instr_id_map: &std::collections::HashMap<InstrId, InstrId>,
    bid_map: &[Option<BlockId>],
    fallback_bid: BlockId,
    _func: &Function,
) -> InstrKind {
    let remap_vref = |v: ValueRef| -> ValueRef {
        match v {
            ValueRef::Instruction(iid) => {
                if let Some(&new_iid) = instr_id_map.get(&iid) {
                    ValueRef::Instruction(new_iid)
                } else {
                    // Instruction was removed; substitute undef is not ideal here
                    // but we handle this through the phi-drop logic below.
                    v
                }
            }
            other => other,
        }
    };
    let remap_bid = |b: BlockId| -> BlockId {
        bid_map.get(b.0 as usize).and_then(|opt| *opt).unwrap_or(fallback_bid)
    };

    match kind {
        InstrKind::Phi { ty, incoming } => {
            let new_incoming: Vec<(ValueRef, BlockId)> = incoming
                .iter()
                .filter_map(|(v, b)| {
                    // Drop incoming edges from removed blocks.
                    let mapped_b = bid_map.get(b.0 as usize).and_then(|opt| *opt);
                    mapped_b.map(|new_b| (remap_vref(*v), new_b))
                })
                .collect();
            InstrKind::Phi {
                ty: *ty,
                incoming: new_incoming,
            }
        }
        InstrKind::Br { dest } => InstrKind::Br {
            dest: remap_bid(*dest),
        },
        InstrKind::CondBr {
            cond,
            then_dest,
            else_dest,
        } => InstrKind::CondBr {
            cond: remap_vref(*cond),
            then_dest: remap_bid(*then_dest),
            else_dest: remap_bid(*else_dest),
        },
        InstrKind::Switch { val, default, cases } => InstrKind::Switch {
            val: remap_vref(*val),
            default: remap_bid(*default),
            cases: cases
                .iter()
                .map(|(v, b)| (remap_vref(*v), remap_bid(*b)))
                .collect(),
        },
        InstrKind::Invoke {
            callee_ty,
            callee,
            args,
            normal_dest,
            unwind_dest,
        } => InstrKind::Invoke {
            callee_ty: *callee_ty,
            callee: remap_vref(*callee),
            args: args.iter().map(|&a| remap_vref(a)).collect(),
            normal_dest: remap_bid(*normal_dest),
            unwind_dest: remap_bid(*unwind_dest),
        },
        _ => {
            // For all other instructions just remap value refs.
            remap_value_refs_in_kind(kind, remap_vref)
        }
    }
}

/// Remap all `ValueRef` operands in an `InstrKind` using a closure, leaving
/// block ids untouched.  Used for the non-branching instructions.
fn remap_value_refs_in_kind<F>(kind: &InstrKind, mut f: F) -> InstrKind
where
    F: FnMut(ValueRef) -> ValueRef,
{
    use InstrKind::*;
    match kind {
        Add { flags, lhs, rhs } => Add { flags: *flags, lhs: f(*lhs), rhs: f(*rhs) },
        Sub { flags, lhs, rhs } => Sub { flags: *flags, lhs: f(*lhs), rhs: f(*rhs) },
        Mul { flags, lhs, rhs } => Mul { flags: *flags, lhs: f(*lhs), rhs: f(*rhs) },
        UDiv { exact, lhs, rhs } => UDiv { exact: *exact, lhs: f(*lhs), rhs: f(*rhs) },
        SDiv { exact, lhs, rhs } => SDiv { exact: *exact, lhs: f(*lhs), rhs: f(*rhs) },
        URem { lhs, rhs } => URem { lhs: f(*lhs), rhs: f(*rhs) },
        SRem { lhs, rhs } => SRem { lhs: f(*lhs), rhs: f(*rhs) },
        And { lhs, rhs } => And { lhs: f(*lhs), rhs: f(*rhs) },
        Or { lhs, rhs } => Or { lhs: f(*lhs), rhs: f(*rhs) },
        Xor { lhs, rhs } => Xor { lhs: f(*lhs), rhs: f(*rhs) },
        Shl { flags, lhs, rhs } => Shl { flags: *flags, lhs: f(*lhs), rhs: f(*rhs) },
        LShr { exact, lhs, rhs } => LShr { exact: *exact, lhs: f(*lhs), rhs: f(*rhs) },
        AShr { exact, lhs, rhs } => AShr { exact: *exact, lhs: f(*lhs), rhs: f(*rhs) },
        FAdd { flags, lhs, rhs } => FAdd { flags: *flags, lhs: f(*lhs), rhs: f(*rhs) },
        FSub { flags, lhs, rhs } => FSub { flags: *flags, lhs: f(*lhs), rhs: f(*rhs) },
        FMul { flags, lhs, rhs } => FMul { flags: *flags, lhs: f(*lhs), rhs: f(*rhs) },
        FDiv { flags, lhs, rhs } => FDiv { flags: *flags, lhs: f(*lhs), rhs: f(*rhs) },
        FRem { flags, lhs, rhs } => FRem { flags: *flags, lhs: f(*lhs), rhs: f(*rhs) },
        FNeg { flags, operand } => FNeg { flags: *flags, operand: f(*operand) },
        ICmp { pred, lhs, rhs } => ICmp { pred: *pred, lhs: f(*lhs), rhs: f(*rhs) },
        FCmp { flags, pred, lhs, rhs } => FCmp {
            flags: *flags,
            pred: *pred,
            lhs: f(*lhs),
            rhs: f(*rhs),
        },
        Alloca { alloc_ty, num_elements, align } => Alloca {
            alloc_ty: *alloc_ty,
            num_elements: num_elements.map(|v| f(v)),
            align: *align,
        },
        Load { ty, ptr, align, volatile } => Load {
            ty: *ty,
            ptr: f(*ptr),
            align: *align,
            volatile: *volatile,
        },
        Store { val, ptr, align, volatile } => Store {
            val: f(*val),
            ptr: f(*ptr),
            align: *align,
            volatile: *volatile,
        },
        GetElementPtr { inbounds, base_ty, ptr, indices } => GetElementPtr {
            inbounds: *inbounds,
            base_ty: *base_ty,
            ptr: f(*ptr),
            indices: indices.iter().map(|&i| f(i)).collect(),
        },
        Trunc { val, to } => Trunc { val: f(*val), to: *to },
        ZExt { val, to } => ZExt { val: f(*val), to: *to },
        SExt { val, to } => SExt { val: f(*val), to: *to },
        FPTrunc { val, to } => FPTrunc { val: f(*val), to: *to },
        FPExt { val, to } => FPExt { val: f(*val), to: *to },
        FPToUI { val, to } => FPToUI { val: f(*val), to: *to },
        FPToSI { val, to } => FPToSI { val: f(*val), to: *to },
        UIToFP { val, to } => UIToFP { val: f(*val), to: *to },
        SIToFP { val, to } => SIToFP { val: f(*val), to: *to },
        PtrToInt { val, to } => PtrToInt { val: f(*val), to: *to },
        IntToPtr { val, to } => IntToPtr { val: f(*val), to: *to },
        BitCast { val, to } => BitCast { val: f(*val), to: *to },
        AddrSpaceCast { val, to } => AddrSpaceCast { val: f(*val), to: *to },
        Freeze { val } => Freeze { val: f(*val) },
        Select { cond, then_val, else_val } => Select {
            cond: f(*cond),
            then_val: f(*then_val),
            else_val: f(*else_val),
        },
        ExtractValue { aggregate, indices } => ExtractValue {
            aggregate: f(*aggregate),
            indices: indices.clone(),
        },
        InsertValue { aggregate, val, indices } => InsertValue {
            aggregate: f(*aggregate),
            val: f(*val),
            indices: indices.clone(),
        },
        ExtractElement { vec, idx } => ExtractElement {
            vec: f(*vec),
            idx: f(*idx),
        },
        InsertElement { vec, val, idx } => InsertElement {
            vec: f(*vec),
            val: f(*val),
            idx: f(*idx),
        },
        ShuffleVector { v1, v2, mask } => ShuffleVector {
            v1: f(*v1),
            v2: f(*v2),
            mask: mask.clone(),
        },
        Call { tail, callee_ty, callee, args } => Call {
            tail: *tail,
            callee_ty: *callee_ty,
            callee: f(*callee),
            args: args.iter().map(|&a| f(a)).collect(),
        },
        LandingPad { result_ty, personality_fn, cleanup, clauses } => LandingPad {
            result_ty: *result_ty,
            personality_fn: personality_fn.map(|v| f(v)),
            cleanup: *cleanup,
            clauses: clauses
                .iter()
                .map(|c| match c {
                    crate::instruction::LandingPadClause::Catch { ty, value } => {
                        crate::instruction::LandingPadClause::Catch { ty: *ty, value: f(*value) }
                    }
                    crate::instruction::LandingPadClause::Filter { ty, value } => {
                        crate::instruction::LandingPadClause::Filter { ty: *ty, value: f(*value) }
                    }
                })
                .collect(),
        },
        InlineAsm { asm_string, constraints, side_effect, align_stack, args } => InlineAsm {
            asm_string: asm_string.clone(),
            constraints: constraints.clone(),
            side_effect: *side_effect,
            align_stack: *align_stack,
            args: args.iter().map(|&a| f(a)).collect(),
        },
        Fence { ordering } => Fence { ordering: *ordering },
        CmpXchg { ptr, cmp, new_val, success_ord, fail_ord, weak, volatile } => CmpXchg {
            ptr: f(*ptr),
            cmp: f(*cmp),
            new_val: f(*new_val),
            success_ord: *success_ord,
            fail_ord: *fail_ord,
            weak: *weak,
            volatile: *volatile,
        },
        AtomicRmw { op, ptr, val, ordering, volatile } => AtomicRmw {
            op: *op,
            ptr: f(*ptr),
            val: f(*val),
            ordering: *ordering,
            volatile: *volatile,
        },
        Ret { val } => Ret { val: val.map(|v| f(v)) },
        Br { dest } => Br { dest: *dest },
        CondBr { cond, then_dest, else_dest } => CondBr {
            cond: f(*cond),
            then_dest: *then_dest,
            else_dest: *else_dest,
        },
        Switch { val, default, cases } => Switch {
            val: f(*val),
            default: *default,
            cases: cases.iter().map(|(v, b)| (f(*v), *b)).collect(),
        },
        Invoke { callee_ty, callee, args, normal_dest, unwind_dest } => Invoke {
            callee_ty: *callee_ty,
            callee: f(*callee),
            args: args.iter().map(|&a| f(a)).collect(),
            normal_dest: *normal_dest,
            unwind_dest: *unwind_dest,
        },
        Resume { val } => Resume { val: f(*val) },
        Unreachable => Unreachable,
        Phi { ty, incoming } => Phi {
            ty: *ty,
            incoming: incoming.iter().map(|(v, b)| (f(*v), *b)).collect(),
        },
    }
}

/// Substitute every occurrence of `old_vref` with `new_vref` in an
/// `InstrKind`, also remapping `InstrId`s through `instr_id_map`.
fn substitute_value_in_kind(
    kind: &InstrKind,
    old_vref: ValueRef,
    new_vref: ValueRef,
    instr_id_map: &std::collections::HashMap<InstrId, InstrId>,
) -> InstrKind {
    let subst = |v: ValueRef| -> ValueRef {
        if v == old_vref {
            return new_vref;
        }
        // Also remap surviving InstrId references.
        if let ValueRef::Instruction(iid) = v {
            if let Some(&new_iid) = instr_id_map.get(&iid) {
                return ValueRef::Instruction(new_iid);
            }
        }
        v
    };
    remap_value_refs_in_kind(kind, subst)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::basic_block::BasicBlock;
    use crate::context::{ArgId, Context, InstrId, ValueRef};
    use crate::function::Function;
    use crate::instruction::{InstrKind, Instruction, IntArithFlags};
    use crate::module::Module;
    use crate::value::{Argument, Linkage};

    /// Build a simple module with two functions:
    ///   - `foo(i32 %a, i32 %b) -> i32`: computes `%r = add i32 %a, %b; ret %r`
    ///   - `bar() -> i32`: just `ret i32 42`
    fn two_function_module() -> (Context, Module) {
        let mut ctx = Context::new();

        // Function foo
        let foo_ty = ctx.mk_fn_type(ctx.i32_ty, vec![ctx.i32_ty, ctx.i32_ty], false);
        let foo_args = vec![
            Argument { name: "a".into(), ty: ctx.i32_ty, index: 0 },
            Argument { name: "b".into(), ty: ctx.i32_ty, index: 1 },
        ];
        let mut foo = Function::new("foo", foo_ty, foo_args, Linkage::External);
        let a_ref = ValueRef::Argument(ArgId(0));
        let b_ref = ValueRef::Argument(ArgId(1));
        let add_id = foo.alloc_instr(Instruction::new(
            Some("r".into()),
            ctx.i32_ty,
            InstrKind::Add { flags: IntArithFlags::default(), lhs: a_ref, rhs: b_ref },
        ));
        let ret_id = foo.alloc_instr(Instruction::new(
            None,
            ctx.void_ty,
            InstrKind::Ret { val: Some(ValueRef::Instruction(add_id)) },
        ));
        let mut entry_foo = BasicBlock::new("entry");
        entry_foo.append_instr(add_id);
        entry_foo.set_terminator(ret_id);
        foo.add_block(entry_foo);

        // Function bar
        let bar_ty = ctx.mk_fn_type(ctx.i32_ty, vec![], false);
        let mut bar = Function::new("bar", bar_ty, vec![], Linkage::External);
        let c42 = ctx.const_int(ctx.i32_ty, 42);
        let bar_ret_id = bar.alloc_instr(Instruction::new(
            None,
            ctx.void_ty,
            InstrKind::Ret { val: Some(ValueRef::Constant(c42)) },
        ));
        let mut entry_bar = BasicBlock::new("entry");
        entry_bar.set_terminator(bar_ret_id);
        bar.add_block(entry_bar);

        let mut module = Module::new("test");
        module.add_function(foo);
        module.add_function(bar);

        (ctx, module)
    }

    // -----------------------------------------------------------------------
    // Test 1: reduce_removes_unused_function
    // -----------------------------------------------------------------------

    #[test]
    fn reduce_removes_unused_function() {
        let (ctx, module) = two_function_module();
        assert_eq!(module.functions.len(), 2);

        // Predicate: IR must contain "foo" (the first function).
        // The second function "bar" is not required.
        let pred = ContainsPredicate { substring: "@foo".to_string() };
        let reducer = Reducer::new();
        let (_ctx2, module2) = reducer.reduce(ctx, module, &pred);

        // After reduction, "bar" should be removed.
        assert!(
            module2.get_function_id("bar").is_none(),
            "expected 'bar' to be removed"
        );
        assert!(
            module2.get_function_id("foo").is_some(),
            "expected 'foo' to be retained"
        );
    }

    // -----------------------------------------------------------------------
    // Test 2: reduce_removes_dead_instruction
    // -----------------------------------------------------------------------

    #[test]
    fn reduce_removes_dead_instruction() {
        // Build a function with a dead arithmetic instruction:
        //   define i32 @compute(i32 %x) {
        //     %dead = mul i32 %x, %x   ; not used anywhere
        //     ret i32 %x
        //   }
        let mut ctx = Context::new();
        let fn_ty = ctx.mk_fn_type(ctx.i32_ty, vec![ctx.i32_ty], false);
        let args = vec![Argument { name: "x".into(), ty: ctx.i32_ty, index: 0 }];
        let mut func = Function::new("compute", fn_ty, args, Linkage::External);
        let x_ref = ValueRef::Argument(ArgId(0));
        let dead_id = func.alloc_instr(Instruction::new(
            Some("dead".into()),
            ctx.i32_ty,
            InstrKind::Mul { flags: IntArithFlags::default(), lhs: x_ref, rhs: x_ref },
        ));
        let ret_id = func.alloc_instr(Instruction::new(
            None,
            ctx.void_ty,
            InstrKind::Ret { val: Some(x_ref) },
        ));
        let mut entry = BasicBlock::new("entry");
        entry.append_instr(dead_id);
        entry.set_terminator(ret_id);
        func.add_block(entry);

        let mut module = Module::new("test");
        module.add_function(func);

        // Predicate: IR must contain "ret".  The dead mul is not needed.
        let pred = ContainsPredicate { substring: "ret".to_string() };
        let reducer = Reducer::new();
        let (_ctx2, module2) = reducer.reduce(ctx, module, &pred);

        // The dead mul should have been removed.
        let f = &module2.functions[0];
        let has_mul = f.instructions.iter().any(|i| matches!(i.kind, InstrKind::Mul { .. }));
        assert!(!has_mul, "expected dead mul to be removed");
    }

    // -----------------------------------------------------------------------
    // Test 3: reduce_replaces_with_undef
    // -----------------------------------------------------------------------

    #[test]
    fn reduce_replaces_with_undef() {
        // Build:
        //   define i32 @foo() {
        //     %a = add i32 0, 1     ; result unused
        //     ret i32 0
        //   }
        // The predicate is "contains 'ret'"; the reducer should be able to
        // remove %a and replace its uses (none) with undef.
        let mut ctx = Context::new();
        let fn_ty = ctx.mk_fn_type(ctx.i32_ty, vec![], false);
        let mut func = Function::new("foo", fn_ty, vec![], Linkage::External);
        let c0 = ctx.const_int(ctx.i32_ty, 0);
        let c1 = ctx.const_int(ctx.i32_ty, 1);
        let add_id = func.alloc_instr(Instruction::new(
            Some("a".into()),
            ctx.i32_ty,
            InstrKind::Add {
                flags: IntArithFlags::default(),
                lhs: ValueRef::Constant(c0),
                rhs: ValueRef::Constant(c1),
            },
        ));
        let ret_id = func.alloc_instr(Instruction::new(
            None,
            ctx.void_ty,
            InstrKind::Ret { val: Some(ValueRef::Constant(c0)) },
        ));
        let mut entry = BasicBlock::new("entry");
        entry.append_instr(add_id);
        entry.set_terminator(ret_id);
        func.add_block(entry);

        let mut module = Module::new("test");
        module.add_function(func);

        let pred = ContainsPredicate { substring: "ret".to_string() };
        let reducer = Reducer::new();
        let (_ctx2, module2) = reducer.reduce(ctx, module, &pred);

        // The add should have been removed (its result was never used).
        let f = &module2.functions[0];
        let has_add = f.instructions.iter().any(|i| matches!(i.kind, InstrKind::Add { .. }));
        assert!(!has_add, "expected add instruction to be removed");
    }

    // -----------------------------------------------------------------------
    // Test 4: reduce_replaces_with_constant
    // -----------------------------------------------------------------------

    #[test]
    fn reduce_replaces_with_constant() {
        // Build:
        //   define i32 @foo(i32 %x) {
        //     %a = add i32 %x, %x
        //     ret i32 %a
        //   }
        // Predicate: "contains 'ret'".  The reducer can replace %a with i32 0
        // and remove the add (since ret i32 0 still satisfies the predicate).
        let mut ctx = Context::new();
        let fn_ty = ctx.mk_fn_type(ctx.i32_ty, vec![ctx.i32_ty], false);
        let args = vec![Argument { name: "x".into(), ty: ctx.i32_ty, index: 0 }];
        let mut func = Function::new("foo", fn_ty, args, Linkage::External);
        let x_ref = ValueRef::Argument(ArgId(0));
        let add_id = func.alloc_instr(Instruction::new(
            Some("a".into()),
            ctx.i32_ty,
            InstrKind::Add { flags: IntArithFlags::default(), lhs: x_ref, rhs: x_ref },
        ));
        let ret_id = func.alloc_instr(Instruction::new(
            None,
            ctx.void_ty,
            InstrKind::Ret { val: Some(ValueRef::Instruction(add_id)) },
        ));
        let mut entry = BasicBlock::new("entry");
        entry.append_instr(add_id);
        entry.set_terminator(ret_id);
        func.add_block(entry);

        let mut module = Module::new("test");
        module.add_function(func);

        let pred = ContainsPredicate { substring: "ret".to_string() };
        let reducer = Reducer::new();
        let (_ctx2, module2) = reducer.reduce(ctx, module, &pred);

        // The add should have been reduced away (replaced by constant zero).
        let f = &module2.functions[0];
        let has_add = f.instructions.iter().any(|i| matches!(i.kind, InstrKind::Add { .. }));
        assert!(!has_add, "expected add to be replaced by constant");
    }

    // -----------------------------------------------------------------------
    // Test 5: reduce_fixed_point
    // -----------------------------------------------------------------------

    #[test]
    fn reduce_fixed_point() {
        let (ctx, module) = two_function_module();

        // Predicate: contains "foo".
        let pred = ContainsPredicate { substring: "@foo".to_string() };
        let reducer = Reducer::new();
        let (ctx1, module1) = reducer.reduce(ctx, module, &pred);

        // Run a second time — should produce the same result (fixed point).
        let text1 = Printer::new(&ctx1).print_module(&module1);
        let (ctx2, module2) = reducer.reduce(ctx1, module1, &pred);
        let text2 = Printer::new(&ctx2).print_module(&module2);

        assert_eq!(text1, text2, "second reduction pass changed the output (not at fixed point)");
    }

    // -----------------------------------------------------------------------
    // Test 6: reduce_preserves_predicate
    // -----------------------------------------------------------------------

    #[test]
    fn reduce_preserves_predicate() {
        let (ctx, module) = two_function_module();

        let pred = ContainsPredicate { substring: "@foo".to_string() };
        let reducer = Reducer::new();
        let (ctx2, module2) = reducer.reduce(ctx, module, &pred);

        // After reduction, the predicate must still hold.
        assert!(
            pred.test(&ctx2, &module2),
            "predicate no longer holds after reduction"
        );
    }

    // -----------------------------------------------------------------------
    // Test 7: contains_predicate
    // -----------------------------------------------------------------------

    #[test]
    fn contains_predicate() {
        // Build a module with an add instruction.
        let mut ctx = Context::new();
        let fn_ty = ctx.mk_fn_type(ctx.i32_ty, vec![ctx.i32_ty, ctx.i32_ty], false);
        let args = vec![
            Argument { name: "a".into(), ty: ctx.i32_ty, index: 0 },
            Argument { name: "b".into(), ty: ctx.i32_ty, index: 1 },
        ];
        let mut func = Function::new("add", fn_ty, args, Linkage::External);
        let a_ref = ValueRef::Argument(ArgId(0));
        let b_ref = ValueRef::Argument(ArgId(1));
        let add_id = func.alloc_instr(Instruction::new(
            Some("r".into()),
            ctx.i32_ty,
            InstrKind::Add { flags: IntArithFlags::default(), lhs: a_ref, rhs: b_ref },
        ));
        let ret_id = func.alloc_instr(Instruction::new(
            None,
            ctx.void_ty,
            InstrKind::Ret { val: Some(ValueRef::Instruction(add_id)) },
        ));
        let mut entry = BasicBlock::new("entry");
        entry.append_instr(add_id);
        entry.set_terminator(ret_id);
        func.add_block(entry);

        let mut module = Module::new("test");
        module.add_function(func);

        let pred = ContainsPredicate { substring: "add".to_string() };
        assert!(
            pred.test(&ctx, &module),
            "ContainsPredicate should return true when module has an add instruction"
        );

        let pred_false = ContainsPredicate { substring: "nonexistent_xyz".to_string() };
        assert!(
            !pred_false.test(&ctx, &module),
            "ContainsPredicate should return false for absent substring"
        );
    }
}
