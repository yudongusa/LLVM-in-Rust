//! LLVM IR → WebAssembly lowering.
//!
//! Uses the "all-locals" strategy: every SSA value becomes a wasm `local`.
//! Arguments occupy the first N locals (indices 0..N).  Instruction results
//! are assigned the next locals.  A simple block-order traversal emits wasm
//! instructions directly; structured control flow is produced by wrapping
//! basic blocks in `block`/`loop` constructs.

use crate::encode::{
    emit_block, emit_br, emit_br_if, emit_call, emit_const, emit_end, emit_if, emit_local_get,
    emit_local_set, emit_loop, emit_return, leb128_u,
};
use crate::instructions::*;
use crate::types::{ir_ty_to_valtype, is_i64_type, VALTYPE_I32};
use llvm_codegen::isel::{IselBackend, MachineFunction, MOpcode};
use llvm_ir::{
    ArgId, BlockId, ConstantData, Context, Function, InstrId, InstrKind, IntPredicate, Module,
    TypeData, TypeId, ValueRef,
};
use std::collections::HashMap;

// ── wasm-specific function representation ─────────────────────────────────

/// A fully lowered wasm function, ready for binary encoding.
pub struct WasmFunction {
    /// Function name (for export section).
    pub name: String,
    /// Index into `module.functions` (for type-section construction).
    pub func_idx: usize,
    /// Wasm local types in declaration order.
    /// Arguments occupy indices 0..(n_args-1); instruction results follow.
    pub local_types: Vec<u8>,
    /// Raw wasm instruction bytes (without the final `end`).
    pub code: Vec<u8>,
}

// ── wasm backend ──────────────────────────────────────────────────────────

/// WebAssembly instruction-selection backend.
///
/// Implements [`IselBackend`] so the backend can be used through the
/// standard pipeline, but the [`MachineFunction`] it returns is a
/// stub — actual wasm code lives in [`WasmFunction`] produced by
/// [`WasmBackend::lower_module`].
pub struct WasmBackend;

impl WasmBackend {
    /// Create a new `WasmBackend`.
    pub fn new() -> Self {
        WasmBackend
    }

    /// Lower all non-declaration functions in `module` to [`WasmFunction`]s.
    pub fn lower_module(
        &mut self,
        ctx: &Context,
        module: &Module,
    ) -> Vec<WasmFunction> {
        module
            .functions
            .iter()
            .enumerate()
            .filter(|(_, f)| !f.is_declaration)
            .map(|(i, f)| lower_function_to_wasm(ctx, module, f, i))
            .collect()
    }
}

impl Default for WasmBackend {
    fn default() -> Self {
        Self::new()
    }
}

// The `IselBackend` trait impl is a thin shim that returns a stub
// `MachineFunction`.  Callers that want the actual wasm bytes should use
// `lower_module` + `encode::assemble_module`.
impl IselBackend for WasmBackend {
    fn lower_function(
        &mut self,
        _ctx: &Context,
        _module: &Module,
        func: &Function,
    ) -> MachineFunction {
        // Return an empty `MachineFunction` as a stub — wasm doesn't use the
        // register-allocator pipeline.
        MachineFunction::new(func.name.clone())
    }
}

// ── per-function lowering ─────────────────────────────────────────────────

/// Lower a single IR function to a [`WasmFunction`].
fn lower_function_to_wasm(
    ctx: &Context,
    module: &Module,
    func: &Function,
    func_idx: usize,
) -> WasmFunction {
    let mut local_types: Vec<u8> = Vec::new();
    let mut vmap: HashMap<ValueRef, u32> = HashMap::new(); // ValueRef → local index
    let mut code: Vec<u8> = Vec::new();

    // ── assign locals for function arguments ──────────────────────────────
    // Arguments are declared as locals 0..n_args in the wasm function.
    // In the wasm binary format, function parameters ARE the first locals —
    // they do not appear in the local declaration section.  We track their
    // indices in vmap but do not emit them in `local_types`.
    let n_args = func.args.len();
    for i in 0..n_args {
        let vt = ir_ty_to_valtype(ctx, func.ty)
            .unwrap_or(VALTYPE_I32); // fallback (won't be used for void)
        // Use the actual argument type from the function type.
        let arg_vt = if let TypeData::Function(ft) = ctx.get_type(func.ty) {
            ft.params
                .get(i)
                .and_then(|&p| ir_ty_to_valtype(ctx, p))
                .unwrap_or(VALTYPE_I32)
        } else {
            vt
        };
        let _ = arg_vt; // arg locals are implicit (params); not in local_types
        vmap.insert(ValueRef::Argument(ArgId(i as u32)), i as u32);
    }

    // Next local index starts after the args.
    let mut next_local: u32 = n_args as u32;

    // ── first pass: assign local indices for all instruction results ───────
    for bb in &func.blocks {
        for &iid in &bb.body {
            let instr = func.instr(iid);
            // Only value-producing instructions get a local.
            if !is_void_type(ctx, instr.ty) {
                let vt = ir_ty_to_valtype(ctx, instr.ty).unwrap_or(VALTYPE_I32);
                vmap.insert(ValueRef::Instruction(iid), next_local);
                local_types.push(vt);
                next_local += 1;
            }
        }
    }

    // ── build a block-index map: BlockId → index in func.blocks ──────────
    let block_index: HashMap<BlockId, usize> = func
        .blocks
        .iter()
        .enumerate()
        .map(|(i, _bb)| (BlockId(i as u32), i))
        .collect();

    // ── determine which blocks are loop headers (have a back-edge) ─────────
    // A back-edge is a branch to a block that comes earlier in the linear order.
    let mut is_loop_header: Vec<bool> = vec![false; func.blocks.len()];
    for (i, bb) in func.blocks.iter().enumerate() {
        if let Some(tid) = bb.terminator {
            match &func.instr(tid).kind {
                InstrKind::Br { dest } => {
                    let j = block_index[dest];
                    if j <= i {
                        is_loop_header[j] = true;
                    }
                }
                InstrKind::CondBr { then_dest, else_dest, .. } => {
                    for dest in [then_dest, else_dest] {
                        let j = block_index[dest];
                        if j <= i {
                            is_loop_header[j] = true;
                        }
                    }
                }
                _ => {}
            }
        }
    }

    // ── second pass: emit code per basic block ─────────────────────────────
    // Strategy:
    //   - Each block is wrapped in a `block` construct (depth = 1 while inside).
    //   - Loop headers are additionally wrapped in a `loop` construct.
    //   - `br 0` exits the current `block` (branches forward to next block's code).
    //   - `br_if` + `block` implements conditional branches.
    //   - Back-edges branch to the enclosing `loop` construct.
    //
    // For simplicity we use the "wrapping" approach:
    //   - All blocks are wrapped in one large outer `block` for the function.
    //   - Each individual block gets its own `block` wrapper.
    //   - Forward branches are `br N` where N is the number of blocks remaining.
    //
    // For now we implement a simpler direct emission:
    //   - emit all blocks sequentially
    //   - handle Ret, Br (unconditional), CondBr (using br_if+block)
    //   - loop back-edges: wrap in `loop`
    //   - forward jumps: wrap destination handling in a block

    // We use a simple approach: all n blocks are wrapped in n nested `block`
    // constructs.  Block i is at nesting depth (n - i) inside the outer wrapper.
    // To jump to block j from block i:
    //   - if j > i (forward): br (j - i - 1) from inside block i's own `block`
    //   - if j <= i (back-edge): impossible to express with this scheme alone;
    //     use a `loop` wrapper for the loop header.
    //
    // Revised "stackify" approach used here:
    // - Emit one outer `block` for each forward branch target.
    // - For loops: emit a `loop` around the loop body.
    // - This is simplified — for very complex CFGs some terminators may emit
    //   `unreachable`.  TODO: implement full Relooper for arbitrary CFGs.

    emit_code_for_function(
        ctx,
        func,
        &vmap,
        &block_index,
        &is_loop_header,
        &mut code,
        module,
    );

    WasmFunction {
        name: func.name.clone(),
        func_idx,
        local_types,
        code,
    }
}

// ── function code emission ────────────────────────────────────────────────

fn emit_code_for_function(
    ctx: &Context,
    func: &Function,
    vmap: &HashMap<ValueRef, u32>,
    block_index: &HashMap<BlockId, usize>,
    is_loop_header: &[bool],
    code: &mut Vec<u8>,
    module: &Module,
) {
    let n = func.blocks.len();
    if n == 0 {
        return;
    }

    // Collect function indices for call resolution.
    // Map function name → wasm function index among non-declarations.
    // We approximate: use module.functions index directly as a proxy.
    let func_idx_map: HashMap<&str, u32> = module
        .functions
        .iter()
        .enumerate()
        .filter(|(_, f)| !f.is_declaration)
        .enumerate()
        .map(|(wasm_idx, (_, f))| (f.name.as_str(), wasm_idx as u32))
        .collect();

    // Wrap all blocks in a chain of `block` constructs so that
    // `br N` can jump to block at position (current + N + 1).
    // The outermost block corresponds to block 0.
    //
    // Layout:
    //   block  ; block n-1 (outermost)
    //     block  ; block n-2
    //       ...
    //         block  ; block 0 (innermost)
    //           <code for block 0>
    //         end  ; exits block 0, continues to block 1's wrapper
    //         <code for block 1>
    //       end
    //       <code for block 2>
    //       ...
    //   end
    //
    // To branch from block i to block j (j > i), we need to break out of
    // (j - i) block constructs, so `br (j - i - 1)`.
    //
    // For loop back-edges (j <= i), we wrap the loop header in a `loop`
    // construct one level inside its `block`.

    // Emit n-1 nested `block` wrappers.
    for _ in 0..(n - 1) {
        emit_block(code);
    }

    for (i, bb) in func.blocks.iter().enumerate() {
        // If this block is a loop header, wrap it in a `loop`.
        if is_loop_header[i] {
            emit_loop(code);
        }

        // Emit all non-terminator instructions.
        for &iid in &bb.body {
            emit_instr(ctx, func, module, vmap, code, iid, &func_idx_map);
        }

        // Emit the terminator.
        if let Some(tid) = bb.terminator {
            emit_terminator(
                ctx,
                func,
                vmap,
                block_index,
                is_loop_header,
                code,
                tid,
                i,
                n,
                &func_idx_map,
            );
        }

        // Close loop wrapper if this block is a loop header.
        if is_loop_header[i] {
            emit_end(code);
        }

        // Close this block's wrapper (if not the last block).
        if i < n - 1 {
            emit_end(code);
        }
    }

    // Close all remaining wrappers.
    // (We opened n-1 blocks; the loop end above closed some, but the inner
    //  block wrappers for non-last blocks were also closed above.  The
    //  remaining unclosed wrapper is the outermost one for block n-2.)
    // Actually with the layout above we open n-1 blocks at the top and close
    // them one-by-one as we finish each non-last block.  But we need to also
    // close the LAST block's wrapper — there isn't one since we didn't open
    // one for the last block.  So no extra `end` needed here.
}

// ── instruction emission ──────────────────────────────────────────────────

fn resolve_vmap(vmap: &HashMap<ValueRef, u32>, vr: ValueRef) -> u32 {
    vmap.get(&vr).copied().unwrap_or(0)
}

fn emit_value(
    ctx: &Context,
    vmap: &HashMap<ValueRef, u32>,
    code: &mut Vec<u8>,
    vr: ValueRef,
    use_i64: bool,
) {
    match vr {
        ValueRef::Constant(cid) => {
            let imm = match ctx.get_const(cid) {
                ConstantData::Int { val, .. } => *val as i64,
                ConstantData::Float { bits, .. } => *bits as i64,
                ConstantData::Null(_) | ConstantData::ZeroInitializer(_) => 0,
                ConstantData::Undef(_) | ConstantData::Poison(_) => 0,
                _ => 0,
            };
            emit_const(code, use_i64, imm);
        }
        _ => {
            let local_idx = resolve_vmap(vmap, vr);
            emit_local_get(code, local_idx);
        }
    }
}

/// Emit wasm instructions for one IR instruction.
fn emit_instr(
    ctx: &Context,
    func: &Function,
    module: &Module,
    vmap: &HashMap<ValueRef, u32>,
    code: &mut Vec<u8>,
    iid: InstrId,
    func_idx_map: &HashMap<&str, u32>,
) {
    let instr = func.instr(iid);
    let is64 = is_i64_type(ctx, instr.ty);
    let dst_local = vmap.get(&ValueRef::Instruction(iid)).copied();

    // Helper: emit LHS and RHS then a binary opcode, then store result.
    macro_rules! binop {
        ($op:expr, $lhs:expr, $rhs:expr) => {{
            emit_value(ctx, vmap, code, *$lhs, is64);
            emit_value(ctx, vmap, code, *$rhs, is64);
            code.push($op.0 as u8);
            if let Some(d) = dst_local {
                emit_local_set(code, d);
            }
        }};
    }

    use InstrKind::*;
    match &instr.kind {
        // ── arithmetic ────────────────────────────────────────────────────
        Add { lhs, rhs, .. } => {
            binop!(if is64 { WASM_I64_ADD } else { WASM_I32_ADD }, lhs, rhs);
        }
        Sub { lhs, rhs, .. } => {
            binop!(if is64 { WASM_I64_SUB } else { WASM_I32_SUB }, lhs, rhs);
        }
        Mul { lhs, rhs, .. } => {
            binop!(if is64 { WASM_I64_MUL } else { WASM_I32_MUL }, lhs, rhs);
        }
        SDiv { lhs, rhs, .. } => {
            binop!(if is64 { WASM_I64_DIV_S } else { WASM_I32_DIV_S }, lhs, rhs);
        }
        UDiv { lhs, rhs, .. } => {
            binop!(if is64 { WASM_I64_DIV_U } else { WASM_I32_DIV_U }, lhs, rhs);
        }
        SRem { lhs, rhs } => {
            binop!(if is64 { WASM_I64_REM_S } else { WASM_I32_REM_S }, lhs, rhs);
        }
        URem { lhs, rhs } => {
            binop!(if is64 { WASM_I64_REM_U } else { WASM_I32_REM_U }, lhs, rhs);
        }
        And { lhs, rhs } => {
            binop!(if is64 { WASM_I64_AND } else { WASM_I32_AND }, lhs, rhs);
        }
        Or { lhs, rhs } => {
            binop!(if is64 { WASM_I64_OR } else { WASM_I32_OR }, lhs, rhs);
        }
        Xor { lhs, rhs } => {
            binop!(if is64 { WASM_I64_XOR } else { WASM_I32_XOR }, lhs, rhs);
        }
        Shl { lhs, rhs, .. } => {
            binop!(if is64 { WASM_I64_SHL } else { WASM_I32_SHL }, lhs, rhs);
        }
        LShr { lhs, rhs, .. } => {
            binop!(
                if is64 { WASM_I64_SHR_U } else { WASM_I32_SHR_U },
                lhs,
                rhs
            );
        }
        AShr { lhs, rhs, .. } => {
            binop!(
                if is64 { WASM_I64_SHR_S } else { WASM_I32_SHR_S },
                lhs,
                rhs
            );
        }

        // ── comparisons ───────────────────────────────────────────────────
        ICmp { pred, lhs, rhs } => {
            // Result is always i32 (bool) in wasm.
            let lhs_is64 = match *lhs {
                ValueRef::Constant(_) => {
                    // Default to i32 for constant operands.
                    false
                }
                ValueRef::Instruction(iid2) => is_i64_type(ctx, func.instr(iid2).ty),
                ValueRef::Argument(aid) => {
                    if let TypeData::Function(ft) = ctx.get_type(func.ty) {
                        ft.params
                            .get(aid.0 as usize)
                            .map(|&p| is_i64_type(ctx, p))
                            .unwrap_or(false)
                    } else {
                        false
                    }
                }
                _ => false,
            };
            emit_value(ctx, vmap, code, *lhs, lhs_is64);
            emit_value(ctx, vmap, code, *rhs, lhs_is64);
            let op = icmp_opcode(*pred, lhs_is64);
            code.push(op.0 as u8);
            if let Some(d) = dst_local {
                emit_local_set(code, d);
            }
        }

        // ── casts ─────────────────────────────────────────────────────────
        Trunc { val, .. } => {
            // i64 → i32
            let src_is64 = match *val {
                ValueRef::Instruction(iid2) => is_i64_type(ctx, func.instr(iid2).ty),
                ValueRef::Argument(aid) => {
                    if let TypeData::Function(ft) = ctx.get_type(func.ty) {
                        ft.params
                            .get(aid.0 as usize)
                            .map(|&p| is_i64_type(ctx, p))
                            .unwrap_or(false)
                    } else {
                        false
                    }
                }
                ValueRef::Constant(_) => false,
                _ => false,
            };
            emit_value(ctx, vmap, code, *val, src_is64);
            if src_is64 && !is64 {
                code.push(WASM_I32_WRAP_I64.0 as u8);
            }
            // Otherwise nop (same width or smaller, wasm handles implicitly).
            if let Some(d) = dst_local {
                emit_local_set(code, d);
            }
        }
        ZExt { val, .. } | SExt { val, .. } => {
            let src_is64 = match *val {
                ValueRef::Instruction(iid2) => is_i64_type(ctx, func.instr(iid2).ty),
                ValueRef::Constant(_) => false,
                _ => false,
            };
            emit_value(ctx, vmap, code, *val, src_is64);
            if !src_is64 && is64 {
                // zero-extend: i64.extend_i32_u; sign-extend: i64.extend_i32_s
                if matches!(instr.kind, SExt { .. }) {
                    code.push(WASM_I64_EXTEND_I32_S.0 as u8);
                } else {
                    code.push(WASM_I64_EXTEND_I32_U.0 as u8);
                }
            }
            if let Some(d) = dst_local {
                emit_local_set(code, d);
            }
        }

        // ── select ────────────────────────────────────────────────────────
        Select { cond, then_val, else_val } => {
            // Wasm doesn't have a direct select for arbitrary nesting here;
            // we use if/else.
            emit_value(ctx, vmap, code, *cond, false);
            emit_if(code);
            emit_value(ctx, vmap, code, *then_val, is64);
            if let Some(d) = dst_local {
                emit_local_set(code, d);
            }
            // else branch
            code.push(0x05); // `else`
            emit_value(ctx, vmap, code, *else_val, is64);
            if let Some(d) = dst_local {
                emit_local_set(code, d);
            }
            emit_end(code);
        }

        // ── memory ────────────────────────────────────────────────────────
        Load { ptr, .. } => {
            emit_value(ctx, vmap, code, *ptr, false); // ptr is i32 (wasm32)
            let op = if is64 { WASM_I64_LOAD } else { WASM_I32_LOAD };
            code.push(op.0 as u8);
            // alignment and offset (memarg): align=2 (4-byte), offset=0
            leb128_u(code, 2); // alignment hint (log2)
            leb128_u(code, 0); // memory offset
            if let Some(d) = dst_local {
                emit_local_set(code, d);
            }
        }
        Store { val, ptr, .. } => {
            emit_value(ctx, vmap, code, *ptr, false);
            let store_is64 = match *val {
                ValueRef::Instruction(iid2) => is_i64_type(ctx, func.instr(iid2).ty),
                ValueRef::Constant(_) => false,
                _ => false,
            };
            emit_value(ctx, vmap, code, *val, store_is64);
            let op = if store_is64 { WASM_I64_STORE } else { WASM_I32_STORE };
            code.push(op.0 as u8);
            leb128_u(code, 2); // alignment hint
            leb128_u(code, 0); // memory offset
            // Store is void — no local.set
        }

        // ── alloca (stub — wasm32 linear memory model) ───────────────────
        Alloca { .. } => {
            // Alloca is not directly supported in wasm; emit 0 as a placeholder
            // address.  TODO: implement stack frame allocation via a global SP.
            emit_const(code, false, 0);
            if let Some(d) = dst_local {
                emit_local_set(code, d);
            }
        }

        // ── GEP (stub) ────────────────────────────────────────────────────
        GetElementPtr { ptr, indices, .. } => {
            // Simplified: emit ptr + (first_index * 4) as i32.
            emit_value(ctx, vmap, code, *ptr, false);
            if let Some(&idx) = indices.first() {
                emit_value(ctx, vmap, code, idx, false);
                emit_const(code, false, 4); // element size hint
                code.push(WASM_I32_MUL.0 as u8);
                code.push(WASM_I32_ADD.0 as u8);
            }
            if let Some(d) = dst_local {
                emit_local_set(code, d);
            }
        }

        // ── phi (handled at block entry via predecessors — stub) ──────────
        Phi { .. } => {
            // Phi values are resolved by the predecessor block's branch.
            // For our simple lowering: emit a `local.get 0` as placeholder —
            // the local was already set by the incoming edge code.
            // TODO: implement proper phi-destruction for loops.
            if let Some(d) = dst_local {
                emit_const(code, is64, 0);
                emit_local_set(code, d);
            }
        }

        // ── call ──────────────────────────────────────────────────────────
        Call { callee, args, .. } => {
            // Resolve callee name.
            let callee_name = match callee {
                ValueRef::Constant(cid) => match ctx.get_const(*cid) {
                    ConstantData::GlobalRef { name, .. } => Some(name.as_str()),
                    _ => None,
                },
                ValueRef::Global(gid) => {
                    // Look up by function index in the module.
                    module.functions.get(gid.0 as usize).map(|f| f.name.as_str())
                }
                _ => None,
            };

            let wasm_fn_idx = callee_name.and_then(|n| func_idx_map.get(n).copied());

            if let Some(fi) = wasm_fn_idx {
                // Push args onto the stack.
                for &arg in args {
                    let arg_is64 = match arg {
                        ValueRef::Instruction(iid2) => is_i64_type(ctx, func.instr(iid2).ty),
                        _ => false,
                    };
                    emit_value(ctx, vmap, code, arg, arg_is64);
                }
                emit_call(code, fi);
                // If the call has a result, store it.
                if let Some(d) = dst_local {
                    emit_local_set(code, d);
                }
            } else {
                // Unknown callee — emit unreachable.
                // TODO: handle indirect calls and external functions via imports.
                code.push(WASM_UNREACHABLE.0 as u8);
                if let Some(d) = dst_local {
                    emit_const(code, is64, 0);
                    emit_local_set(code, d);
                }
            }
        }

        // ── bitcast / ptrtoint / inttoptr / addrspacecast ─────────────────
        BitCast { val, .. }
        | PtrToInt { val, .. }
        | IntToPtr { val, .. }
        | AddrSpaceCast { val, .. }
        | Freeze { val } => {
            let src_is64 = match *val {
                ValueRef::Instruction(iid2) => is_i64_type(ctx, func.instr(iid2).ty),
                _ => false,
            };
            emit_value(ctx, vmap, code, *val, src_is64);
            if let Some(d) = dst_local {
                emit_local_set(code, d);
            }
        }

        // ── no-ops for unsupported instructions ───────────────────────────
        _ => {
            // Unsupported: push a zero constant for the result if any.
            // TODO: Add support for FP, vectors, atomics, etc.
            if let Some(d) = dst_local {
                emit_const(code, is64, 0);
                emit_local_set(code, d);
            }
        }
    }
}

// ── terminator emission ───────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn emit_terminator(
    ctx: &Context,
    func: &Function,
    vmap: &HashMap<ValueRef, u32>,
    block_index: &HashMap<BlockId, usize>,
    _is_loop_header: &[bool],
    code: &mut Vec<u8>,
    tid: InstrId,
    current_block: usize,
    _total_blocks: usize,
    _func_idx_map: &HashMap<&str, u32>,
) {
    let instr = func.instr(tid);
    use InstrKind::*;
    match &instr.kind {
        Ret { val } => {
            if let Some(v) = val {
                let is64 = match *v {
                    ValueRef::Instruction(iid2) => is_i64_type(ctx, func.instr(iid2).ty),
                    ValueRef::Constant(_) => {
                        // Check the function return type.
                        if let TypeData::Function(ft) = ctx.get_type(func.ty) {
                            is_i64_type(ctx, ft.ret)
                        } else {
                            false
                        }
                    }
                    _ => false,
                };
                emit_value(ctx, vmap, code, *v, is64);
            }
            emit_return(code);
        }

        Br { dest } => {
            let target = block_index[dest];
            if target <= current_block {
                // Back-edge: branch to the loop construct.
                // The loop is at depth 1 inside this block's `block` wrapper.
                // If the current block is itself a loop header, the loop is depth 0.
                // For simplicity: `br 0` to the innermost loop.
                // This works when there is only one loop level.
                code.push(WASM_BR.0 as u8);
                // Depth: need to escape the current block's `block` wrapper (depth 1)
                // to reach the loop construct (depth 0 inside the loop wrapper).
                // With our layout: br 0 exits to the loop construct.
                let_leb128_u(code, 0u64);
            } else {
                // Forward branch.
                // We need to exit (target - current_block) `block` constructs
                // to land just before block `target`'s code.
                // Each block from current+1 to target-1 has been wrapped in a block;
                // exiting them all puts us at block `target`.
                // br depth = (target - current_block - 1) exits the right number.
                let depth = target - current_block - 1;
                emit_br(code, depth as u32);
            }
        }

        CondBr { cond, then_dest, else_dest } => {
            let then_target = block_index[then_dest];
            let else_target = block_index[else_dest];

            // Push condition.
            emit_value(ctx, vmap, code, *cond, false);

            if then_target == current_block + 1 && else_target > current_block + 1 {
                // then falls through, else branches forward.
                // Use `br_if 0` to skip over the else-branch block.
                // Actually: push cond, br_if to then_target means "if true, branch".
                // We want "if false, skip to else".
                // Simplest: negate cond with i32.eqz, then br_if to else.
                code.push(WASM_I32_EQZ.0 as u8);
                let depth = else_target - current_block - 1;
                emit_br_if(code, depth as u32);
                // Fall through to then_target (next block).
            } else if else_target == current_block + 1 && then_target > current_block + 1 {
                // else falls through, then branches forward.
                let depth = then_target - current_block - 1;
                emit_br_if(code, depth as u32);
                // Fall through to else_target (next block).
            } else {
                // Both branch forward or back. Use if/else construct.
                // Push the condition (already pushed above).
                emit_if(code);
                // then branch
                if then_target == current_block + 1 {
                    // fall through — nothing to emit for unconditional fallthrough
                } else if then_target <= current_block {
                    code.push(WASM_BR.0 as u8);
                    let_leb128_u(code, 0u64);
                } else {
                    let depth = then_target - current_block - 1;
                    emit_br(code, depth as u32);
                }
                // else branch
                code.push(0x05); // `else`
                if else_target == current_block + 1 {
                    // fall through
                } else if else_target <= current_block {
                    code.push(WASM_BR.0 as u8);
                    let_leb128_u(code, 0u64);
                } else {
                    let depth = else_target - current_block - 1;
                    emit_br(code, depth as u32);
                }
                emit_end(code);
            }
        }

        Switch { val, default, cases } => {
            // TODO: implement switch via br_table.  For now: unconditional branch to default.
            let _ = (val, cases);
            let target = block_index[default];
            if target <= current_block {
                code.push(WASM_BR.0 as u8);
                let_leb128_u(code, 0u64);
            } else {
                let depth = target - current_block - 1;
                emit_br(code, depth as u32);
            }
        }

        Unreachable => {
            code.push(WASM_UNREACHABLE.0 as u8);
        }

        // Ret/terminators handled above; fallthrough is safe.
        _ => {
            // Unexpected: emit unreachable.
            code.push(WASM_UNREACHABLE.0 as u8);
        }
    }
}

// Helper to write a LEB128 u64 without creating a temporary Vec.
#[inline(always)]
fn let_leb128_u(code: &mut Vec<u8>, v: u64) {
    leb128_u(code, v);
}

// ── icmp predicate → wasm opcode ─────────────────────────────────────────

fn icmp_opcode(pred: IntPredicate, is64: bool) -> MOpcode {
    if is64 {
        match pred {
            IntPredicate::Eq => WASM_I64_EQ,
            IntPredicate::Ne => WASM_I64_NE,
            IntPredicate::Slt => WASM_I64_LT_S,
            IntPredicate::Sle => WASM_I64_LE_S,
            IntPredicate::Sgt => WASM_I64_GT_S,
            IntPredicate::Sge => WASM_I64_GE_S,
            IntPredicate::Ult => WASM_I64_LT_U,
            IntPredicate::Ule => WASM_I64_LE_U,
            IntPredicate::Ugt => WASM_I64_GT_U,
            IntPredicate::Uge => WASM_I64_GE_U,
        }
    } else {
        match pred {
            IntPredicate::Eq => WASM_I32_EQ,
            IntPredicate::Ne => WASM_I32_NE,
            IntPredicate::Slt => WASM_I32_LT_S,
            IntPredicate::Sle => WASM_I32_LE_S,
            IntPredicate::Sgt => WASM_I32_GT_S,
            IntPredicate::Sge => WASM_I32_GE_S,
            IntPredicate::Ult => WASM_I32_LT_U,
            IntPredicate::Ule => WASM_I32_LE_U,
            IntPredicate::Ugt => WASM_I32_GT_U,
            IntPredicate::Uge => WASM_I32_GE_U,
        }
    }
}

// ── misc helpers ──────────────────────────────────────────────────────────

fn is_void_type(ctx: &Context, ty: TypeId) -> bool {
    matches!(
        ctx.get_type(ty),
        TypeData::Void | TypeData::Label | TypeData::Metadata
    )
}
