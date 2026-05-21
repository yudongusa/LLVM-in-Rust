//! AddressSanitizer (ASan) instrumentation pass.
//!
//! This pass instruments every `Load` and `Store` instruction in non-declaration
//! functions to check shadow memory before the access, detecting out-of-bounds
//! and use-after-free memory errors.
//!
//! # What it does
//!
//! For each non-declaration function, the pass:
//!
//! 1. **Declares** the ASan runtime functions (e.g. `__asan_report_load4`,
//!    `__asan_report_store4`) in the module if not already declared.
//! 2. **Adds a module constructor** `@__asan_module_ctor` that calls `@__asan_init`.
//! 3. **Instruments every Load/Store** by inserting a shadow-memory check:
//!    - Cast the pointer to `i64` via `ptrtoint`
//!    - Shift right by 3 to get the shadow index
//!    - Add `shadow_offset` to get the shadow address
//!    - Cast back to pointer and load one `i8` byte
//!    - If the shadow byte is non-zero, branch to a report block that calls
//!      `__asan_report_load{N}` or `__asan_report_store{N}`, then continues
//! 4. **Adds stack redzones** around each `alloca` in the entry block:
//!    - A `[32 x i8]` front redzone before each alloca
//!    - A `[32 x i8]` back redzone after each alloca
//!    - Poisons both redzones via `__asan_poison_stack_memory` at function entry

use crate::pass::ModulePass;
use llvm_ir::{
    BasicBlock, BlockId, Context, FunctionId, InstrId, InstrKind, Instruction, IntPredicate,
    Linkage, Module, TailCallKind, ValueRef,
};

/// AddressSanitizer instrumentation pass.
///
/// Inserts shadow-memory checks before every `load` and `store`, adds
/// stack redzones around `alloca`s, and emits an `@__asan_module_ctor`
/// that calls `@__asan_init` at program startup.
#[derive(Debug, Clone)]
pub struct Asan {
    /// Shadow memory offset (Linux x86-64 default: 0x7fff8000).
    pub shadow_offset: i64,
}

impl Default for Asan {
    fn default() -> Self {
        Self {
            shadow_offset: 0x7fff8000,
        }
    }
}

impl ModulePass for Asan {
    fn name(&self) -> &'static str {
        "asan"
    }

    fn run_on_module(&mut self, ctx: &mut Context, module: &mut Module) -> bool {
        // Step 1: declare all ASan runtime functions.
        ensure_declarations(ctx, module);

        // Step 2: add module constructor that calls __asan_init.
        add_module_ctor(ctx, module);

        // Step 3: instrument each non-declaration function.
        let num_funcs = module.functions.len();
        let mut changed = false;
        for fi in 0..num_funcs {
            if module.functions[fi].is_declaration {
                continue;
            }
            changed |= instrument_function(ctx, module, FunctionId(fi as u32), self.shadow_offset);
        }
        changed
    }
}

// ---------------------------------------------------------------------------
// Runtime declarations
// ---------------------------------------------------------------------------

/// Names and signatures of ASan runtime functions to declare.
///
/// Each entry is (name, has_i64_arg) — all are `void` return, either
/// no arguments (`__asan_init`) or one `i64` arg (the address/size).
static ASAN_RUNTIME_FNS: &[(&str, bool, usize)] = &[
    ("__asan_init", false, 0),
    ("__asan_report_load1", true, 1),
    ("__asan_report_load2", true, 1),
    ("__asan_report_load4", true, 1),
    ("__asan_report_load8", true, 1),
    ("__asan_report_store1", true, 1),
    ("__asan_report_store2", true, 1),
    ("__asan_report_store4", true, 1),
    ("__asan_report_store8", true, 1),
    ("__asan_poison_stack_memory", true, 2),
    ("__asan_unpoison_stack_memory", true, 2),
];

fn ensure_declarations(ctx: &mut Context, module: &mut Module) {
    for &(name, _has_arg, nargs) in ASAN_RUNTIME_FNS {
        if module.get_function_id(name).is_some() {
            continue;
        }
        let void_ty = ctx.void_ty;
        let i64_ty = ctx.i64_ty;
        let params: Vec<_> = (0..nargs).map(|_| i64_ty).collect();
        let fn_ty = ctx.mk_fn_type(void_ty, params.clone(), false);
        use llvm_ir::{Argument, Function};
        let args: Vec<Argument> = params
            .iter()
            .enumerate()
            .map(|(i, &ty)| Argument {
                name: String::new(),
                ty,
                index: i as u32,
            })
            .collect();
        let decl = Function::new_declaration(name, fn_ty, args, Linkage::External);
        module.add_function(decl);
    }
}

// ---------------------------------------------------------------------------
// Module constructor
// ---------------------------------------------------------------------------

fn add_module_ctor(ctx: &mut Context, module: &mut Module) {
    const CTOR_NAME: &str = "__asan_module_ctor";
    if module.get_function_id(CTOR_NAME).is_some() {
        return;
    }

    let void_ty = ctx.void_ty;
    let fn_ty = ctx.mk_fn_type(void_ty, vec![], false);
    let mut ctor = llvm_ir::Function::new(CTOR_NAME, fn_ty, vec![], Linkage::External);

    // Build the single basic block.
    let mut entry_bb = BasicBlock::new("entry");

    // Call @__asan_init.
    let init_fid = module
        .get_function_id("__asan_init")
        .expect("__asan_init must be declared first");
    let init_fn_ty = module.functions[init_fid.0 as usize].ty;

    let call_iid = ctor.alloc_instr(Instruction {
        name: None,
        ty: void_ty,
        kind: InstrKind::Call {
            tail: TailCallKind::None,
            callee_ty: init_fn_ty,
            callee: ValueRef::Global(llvm_ir::GlobalId(init_fid.0)),
            args: vec![],
        },
    });
    entry_bb.body.push(call_iid);

    let ret_iid = ctor.alloc_instr(Instruction {
        name: None,
        ty: void_ty,
        kind: InstrKind::Ret { val: None },
    });
    entry_bb.set_terminator(ret_iid);
    ctor.add_block(entry_bb);

    module.add_function(ctor);
}

// ---------------------------------------------------------------------------
// Per-function instrumentation
// ---------------------------------------------------------------------------

fn instrument_function(
    ctx: &mut Context,
    module: &mut Module,
    fid: FunctionId,
    shadow_offset: i64,
) -> bool {
    let mut changed = false;

    // Step A: add stack redzones.
    changed |= add_stack_redzones(ctx, module, fid);

    // Step B: instrument loads and stores.
    changed |= instrument_memory_accesses(ctx, module, fid, shadow_offset);

    changed
}

// ---------------------------------------------------------------------------
// Stack redzones
// ---------------------------------------------------------------------------

fn add_stack_redzones(ctx: &mut Context, module: &mut Module, fid: FunctionId) -> bool {
    // Find alloca positions in the entry block (block 0).
    let func = &module.functions[fid.0 as usize];
    if func.blocks.is_empty() {
        return false;
    }

    // Collect positions of alloca instructions in the entry block.
    let entry_body = func.blocks[0].body.clone();
    let alloca_positions: Vec<usize> = entry_body
        .iter()
        .enumerate()
        .filter_map(|(pos, &iid)| {
            if matches!(func.instr(iid).kind, InstrKind::Alloca { .. }) {
                Some(pos)
            } else {
                None
            }
        })
        .collect();

    if alloca_positions.is_empty() {
        return false;
    }

    // Look up poison function.
    let poison_fid = match module.get_function_id("__asan_poison_stack_memory") {
        Some(id) => id,
        None => return false,
    };

    let i8_ty = ctx.i8_ty;
    let i64_ty = ctx.i64_ty;
    let void_ty = ctx.void_ty;
    let ptr_ty = ctx.ptr_ty;
    let redzone_arr_ty = ctx.mk_array(i8_ty, 32);
    let c32 = ctx.const_int(i64_ty, 32);
    let c32_ref = ValueRef::Constant(c32);

    let poison_fn_ty = module.functions[poison_fid.0 as usize].ty;
    let poison_callee = ValueRef::Global(llvm_ir::GlobalId(poison_fid.0));

    // Process in reverse order so earlier positions remain valid.
    let mut new_body: Vec<InstrId> = entry_body.clone();

    // We'll collect insertion actions (position, list_of_iids) and apply in
    // reverse order.
    struct Insertion {
        before_pos: usize,
        iids: Vec<InstrId>,
    }
    let mut insertions: Vec<Insertion> = Vec::new();

    let func = &mut module.functions[fid.0 as usize];

    for &pos in alloca_positions.iter().rev() {
        let _alloca_iid = new_body[pos];

        // Front redzone: alloca [32 x i8] before the user alloca.
        let front_name = func.fresh_name();
        let front_iid = func.alloc_instr(Instruction {
            name: Some(front_name),
            ty: ptr_ty,
            kind: InstrKind::Alloca {
                alloc_ty: redzone_arr_ty,
                num_elements: None,
                align: Some(32),
            },
        });

        // Back redzone: alloca [32 x i8] after the user alloca.
        let back_name = func.fresh_name();
        let back_iid = func.alloc_instr(Instruction {
            name: Some(back_name),
            ty: ptr_ty,
            kind: InstrKind::Alloca {
                alloc_ty: redzone_arr_ty,
                num_elements: None,
                align: Some(32),
            },
        });

        // ptrtoint front_redzone to i64 for poison call.
        let front_int_name = func.fresh_name();
        let front_int_iid = func.alloc_instr(Instruction {
            name: Some(front_int_name),
            ty: i64_ty,
            kind: InstrKind::PtrToInt {
                val: ValueRef::Instruction(front_iid),
                to: i64_ty,
            },
        });

        // ptrtoint back_redzone to i64 for poison call.
        let back_int_name = func.fresh_name();
        let back_int_iid = func.alloc_instr(Instruction {
            name: Some(back_int_name),
            ty: i64_ty,
            kind: InstrKind::PtrToInt {
                val: ValueRef::Instruction(back_iid),
                to: i64_ty,
            },
        });

        // call @__asan_poison_stack_memory(front_int, 32).
        let poison_front_iid = func.alloc_instr(Instruction {
            name: None,
            ty: void_ty,
            kind: InstrKind::Call {
                tail: TailCallKind::None,
                callee_ty: poison_fn_ty,
                callee: poison_callee,
                args: vec![ValueRef::Instruction(front_int_iid), c32_ref],
            },
        });

        // call @__asan_poison_stack_memory(back_int, 32).
        let poison_back_iid = func.alloc_instr(Instruction {
            name: None,
            ty: void_ty,
            kind: InstrKind::Call {
                tail: TailCallKind::None,
                callee_ty: poison_fn_ty,
                callee: poison_callee,
                args: vec![ValueRef::Instruction(back_int_iid), c32_ref],
            },
        });

        // Insert: [front_redzone] before pos, [back_redzone] after pos,
        // poison calls at the very beginning (entry point).
        // Simplest: insert front before alloca, back after alloca,
        // and poison calls before the front redzone (at the same insertion point).
        //
        // We'll insert them all before pos:
        // [front_iid, <alloca at pos>, back_iid, front_int_iid, back_int_iid,
        //  poison_front_iid, poison_back_iid]
        //
        // To keep things simple, we insert:
        //   Before pos: [front_iid]
        //   After pos (pos+1): [back_iid]
        //   After back (pos+2): [front_int_iid, back_int_iid, poison_front_iid, poison_back_iid]
        insertions.push(Insertion {
            before_pos: pos,
            iids: vec![front_iid],
        });
        insertions.push(Insertion {
            before_pos: pos + 1, // after the alloca (will be adjusted for the front insert)
            iids: vec![
                back_iid,
                front_int_iid,
                back_int_iid,
                poison_front_iid,
                poison_back_iid,
            ],
        });
    }

    // Apply insertions in reverse-position order to keep indices valid.
    // Sort by position descending.
    insertions.sort_by_key(|ins| std::cmp::Reverse(ins.before_pos));

    for ins in &insertions {
        let pos = ins.before_pos.min(new_body.len());
        for (offset, &iid) in ins.iids.iter().enumerate() {
            new_body.insert(pos + offset, iid);
        }
    }

    module.functions[fid.0 as usize].blocks[0].body = new_body;

    true
}

// ---------------------------------------------------------------------------
// Load/Store shadow-memory instrumentation
// ---------------------------------------------------------------------------

fn instrument_memory_accesses(
    ctx: &mut Context,
    module: &mut Module,
    fid: FunctionId,
    shadow_offset: i64,
) -> bool {
    // Collect all (block_idx, body_pos, instr_id) for Load/Store instructions.
    let func = &module.functions[fid.0 as usize];

    struct MemAccess {
        block_idx: usize,
        body_pos: usize,
        iid: InstrId,
        is_load: bool,
        access_bytes: u64, // 1, 2, 4, or 8
    }

    let mut accesses: Vec<MemAccess> = Vec::new();
    for (bi, bb) in func.blocks.iter().enumerate() {
        for (pos, &iid) in bb.body.iter().enumerate() {
            let kind = &func.instr(iid).kind;
            match kind {
                InstrKind::Load { ty, .. } => {
                    let bytes = type_size_bytes(ctx, *ty).unwrap_or(4);
                    accesses.push(MemAccess {
                        block_idx: bi,
                        body_pos: pos,
                        iid,
                        is_load: true,
                        access_bytes: bytes,
                    });
                }
                InstrKind::Store { val, .. } => {
                    let val_ty = value_type(func, *val);
                    let bytes = val_ty
                        .and_then(|ty| type_size_bytes(ctx, ty))
                        .unwrap_or(4);
                    accesses.push(MemAccess {
                        block_idx: bi,
                        body_pos: pos,
                        iid,
                        is_load: false,
                        access_bytes: bytes,
                    });
                }
                _ => {}
            }
        }
    }

    if accesses.is_empty() {
        return false;
    }

    // Look up report functions.
    let get_report_fid = |module: &Module, is_load: bool, bytes: u64| -> Option<FunctionId> {
        let name = if is_load {
            match bytes {
                1 => "__asan_report_load1",
                2 => "__asan_report_load2",
                8 => "__asan_report_load8",
                _ => "__asan_report_load4",
            }
        } else {
            match bytes {
                1 => "__asan_report_store1",
                2 => "__asan_report_store2",
                8 => "__asan_report_store8",
                _ => "__asan_report_store4",
            }
        };
        module.get_function_id(name)
    };

    let i8_ty = ctx.i8_ty;
    let i64_ty = ctx.i64_ty;
    let i1_ty = ctx.i1_ty;
    let void_ty = ctx.void_ty;
    let ptr_ty = ctx.ptr_ty;
    let shadow_off_const = ctx.const_int(i64_ty, shadow_offset as u64);
    let shadow_off_ref = ValueRef::Constant(shadow_off_const);
    let shift3_const = ctx.const_int(i64_ty, 3);
    let shift3_ref = ValueRef::Constant(shift3_const);
    let zero_i8_const = ctx.const_int(i8_ty, 0);
    let zero_i8_ref = ValueRef::Constant(zero_i8_const);

    // Process in reverse order (last block, last position first) so that
    // block indices remain valid as we add new blocks.
    // Sort by (block_idx DESC, body_pos DESC).
    let mut sorted_accesses: Vec<MemAccess> = accesses;
    sorted_accesses.sort_by(|a, b| {
        b.block_idx
            .cmp(&a.block_idx)
            .then(b.body_pos.cmp(&a.body_pos))
    });

    for access in sorted_accesses {
        let MemAccess {
            block_idx,
            body_pos,
            iid: _access_iid,
            is_load,
            access_bytes,
        } = access;

        // Find the report function for this access size.
        let report_fid = match get_report_fid(module, is_load, access_bytes) {
            Some(fid) => fid,
            None => continue,
        };
        let report_fn_ty = module.functions[report_fid.0 as usize].ty;
        let report_callee = ValueRef::Global(llvm_ir::GlobalId(report_fid.0));

        // We need to split block at body_pos:
        //   check_block: [check instrs] → cond_br(shadow_ne_zero, report_block, ok_block)
        //   report_block: [call report, br ok_block]  (new block)
        //   ok_block: [original instr at body_pos, rest of body, terminator]  (new block)

        let func = &mut module.functions[fid.0 as usize];

        // --- Extract the pointer operand of the Load/Store.
        let access_ptr: ValueRef = {
            let kind = &func.instr(func.blocks[block_idx].body[body_pos]).kind;
            match kind {
                InstrKind::Load { ptr, .. } => *ptr,
                InstrKind::Store { ptr, .. } => *ptr,
                _ => continue,
            }
        };

        // Current block's body[0..body_pos] stays in the current block.
        // body[body_pos..] moves to ok_block.
        let ok_body: Vec<InstrId> = func.blocks[block_idx].body[body_pos..].to_vec();
        let ok_term = func.blocks[block_idx].terminator;

        // Truncate current block.
        func.blocks[block_idx].body.truncate(body_pos);
        func.blocks[block_idx].terminator = None;

        // Build new blocks.
        let ok_block_idx = func.blocks.len() as u32;
        let report_block_idx = ok_block_idx + 1;

        // ok_block: original access + rest.
        let mut ok_bb = BasicBlock::new(func.fresh_name());
        ok_bb.body = ok_body;
        ok_bb.terminator = ok_term;

        // report_block: call report, br ok_block.
        let report_bb_name = func.fresh_name();

        // Allocate check instructions into the *current* (check) block.

        // 1. ptrtoint ptr to i64.
        let ptr_int_name = func.fresh_name();
        let ptr_int_iid = func.alloc_instr(Instruction {
            name: Some(ptr_int_name),
            ty: i64_ty,
            kind: InstrKind::PtrToInt {
                val: access_ptr,
                to: i64_ty,
            },
        });
        func.blocks[block_idx].body.push(ptr_int_iid);

        // 2. lshr ptr_int, 3.
        let shadow_idx_name = func.fresh_name();
        let shadow_idx_iid = func.alloc_instr(Instruction {
            name: Some(shadow_idx_name),
            ty: i64_ty,
            kind: InstrKind::LShr {
                exact: false,
                lhs: ValueRef::Instruction(ptr_int_iid),
                rhs: shift3_ref,
            },
        });
        func.blocks[block_idx].body.push(shadow_idx_iid);

        // 3. add shadow_idx, shadow_offset.
        let shadow_addr_name = func.fresh_name();
        let shadow_addr_iid = func.alloc_instr(Instruction {
            name: Some(shadow_addr_name),
            ty: i64_ty,
            kind: InstrKind::Add {
                flags: llvm_ir::IntArithFlags::default(),
                lhs: ValueRef::Instruction(shadow_idx_iid),
                rhs: shadow_off_ref,
            },
        });
        func.blocks[block_idx].body.push(shadow_addr_iid);

        // 4. inttoptr shadow_addr to ptr.
        let shadow_ptr_name = func.fresh_name();
        let shadow_ptr_iid = func.alloc_instr(Instruction {
            name: Some(shadow_ptr_name),
            ty: ptr_ty,
            kind: InstrKind::IntToPtr {
                val: ValueRef::Instruction(shadow_addr_iid),
                to: ptr_ty,
            },
        });
        func.blocks[block_idx].body.push(shadow_ptr_iid);

        // 5. load i8 from shadow_ptr.
        let shadow_byte_name = func.fresh_name();
        let shadow_byte_iid = func.alloc_instr(Instruction {
            name: Some(shadow_byte_name),
            ty: i8_ty,
            kind: InstrKind::Load {
                ty: i8_ty,
                ptr: ValueRef::Instruction(shadow_ptr_iid),
                align: Some(1),
                volatile: false,
            },
        });
        func.blocks[block_idx].body.push(shadow_byte_iid);

        // 6. icmp ne shadow_byte, 0.
        let cmp_name = func.fresh_name();
        let cmp_iid = func.alloc_instr(Instruction {
            name: Some(cmp_name),
            ty: i1_ty,
            kind: InstrKind::ICmp {
                pred: IntPredicate::Ne,
                lhs: ValueRef::Instruction(shadow_byte_iid),
                rhs: zero_i8_ref,
            },
        });
        func.blocks[block_idx].body.push(cmp_iid);

        // 7. cond_br cmp, report_block, ok_block.
        let cond_br_iid = func.alloc_instr(Instruction {
            name: None,
            ty: void_ty,
            kind: InstrKind::CondBr {
                cond: ValueRef::Instruction(cmp_iid),
                then_dest: BlockId(report_block_idx),
                else_dest: BlockId(ok_block_idx),
            },
        });
        func.blocks[block_idx].set_terminator(cond_br_iid);

        // Build report_block.
        let mut report_bb = BasicBlock::new(report_bb_name);

        // call @__asan_report_loadN(ptr_int) or store variant.
        let report_call_iid = func.alloc_instr(Instruction {
            name: None,
            ty: void_ty,
            kind: InstrKind::Call {
                tail: TailCallKind::None,
                callee_ty: report_fn_ty,
                callee: report_callee,
                args: vec![ValueRef::Instruction(ptr_int_iid)],
            },
        });
        report_bb.body.push(report_call_iid);

        // br ok_block.
        let br_ok_iid = func.alloc_instr(Instruction {
            name: None,
            ty: void_ty,
            kind: InstrKind::Br {
                dest: BlockId(ok_block_idx),
            },
        });
        report_bb.set_terminator(br_ok_iid);

        // Append ok_block then report_block.
        // ok_block_idx = current blocks.len()
        func.blocks.push(ok_bb);
        // report_block_idx = ok_block_idx + 1
        func.blocks.push(report_bb);
    }

    true
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Return the byte size of a type for use in selecting the report function.
fn type_size_bytes(ctx: &Context, ty: llvm_ir::TypeId) -> Option<u64> {
    use llvm_ir::TypeData;
    match ctx.get_type(ty) {
        TypeData::Integer(bits) => Some((*bits as u64).div_ceil(8)),
        TypeData::Float(llvm_ir::FloatKind::Single) => Some(4),
        TypeData::Float(llvm_ir::FloatKind::Double) => Some(8),
        TypeData::Pointer => Some(8),
        _ => None,
    }
}

/// Return the type of an SSA value within the function (instruction or argument).
fn value_type(func: &llvm_ir::Function, vref: ValueRef) -> Option<llvm_ir::TypeId> {
    match vref {
        ValueRef::Instruction(iid) => Some(func.instr(iid).ty),
        ValueRef::Argument(aid) => Some(func.arg(aid).ty),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pass::ModulePass;
    use llvm_ir::{Builder, Context, Function, InstrKind, Linkage, Module};

    /// Build a function with `%v = load i32, ptr %p`.
    fn make_load_module() -> (Context, Module) {
        let mut ctx = Context::new();
        let mut module = Module::new("test");
        let mut b = Builder::new(&mut ctx, &mut module);
        let ptr_ty = b.ctx.ptr_ty;
        let i32_ty = b.ctx.i32_ty;
        b.add_function(
            "f",
            i32_ty,
            vec![ptr_ty],
            vec!["p".into()],
            false,
            Linkage::External,
        );
        let entry = b.add_block("entry");
        b.position_at_end(entry);
        let p = b.get_arg(0);
        let v = b.build_load("v", i32_ty, p);
        b.build_ret(v);
        (ctx, module)
    }

    /// Build a function with a `store i32`.
    fn make_store_module() -> (Context, Module) {
        let mut ctx = Context::new();
        let mut module = Module::new("test");
        let mut b = Builder::new(&mut ctx, &mut module);
        let ptr_ty = b.ctx.ptr_ty;
        let i32_ty = b.ctx.i32_ty;
        b.add_function(
            "f",
            b.ctx.void_ty,
            vec![i32_ty, ptr_ty],
            vec!["val".into(), "p".into()],
            false,
            Linkage::External,
        );
        let entry = b.add_block("entry");
        b.position_at_end(entry);
        let val = b.get_arg(0);
        let p = b.get_arg(1);
        b.build_store(val, p);
        b.build_ret_void();
        (ctx, module)
    }

    /// Build a function with one alloca.
    fn make_alloca_module() -> (Context, Module) {
        let mut ctx = Context::new();
        let mut module = Module::new("test");
        let mut b = Builder::new(&mut ctx, &mut module);
        let i32_ty = b.ctx.i32_ty;
        b.add_function("f", b.ctx.void_ty, vec![], vec![], false, Linkage::External);
        let entry = b.add_block("entry");
        b.position_at_end(entry);
        let _slot = b.build_alloca("slot", i32_ty);
        b.build_ret_void();
        (ctx, module)
    }

    #[test]
    fn asan_declares_report_load4() {
        let (mut ctx, mut module) = make_load_module();
        let mut pass = Asan::default();
        pass.run_on_module(&mut ctx, &mut module);
        assert!(
            module.get_function_id("__asan_report_load4").is_some(),
            "__asan_report_load4 must be declared"
        );
    }

    #[test]
    fn asan_declares_asan_init() {
        let (mut ctx, mut module) = make_load_module();
        let mut pass = Asan::default();
        pass.run_on_module(&mut ctx, &mut module);
        assert!(
            module.get_function_id("__asan_init").is_some(),
            "__asan_init must be declared"
        );
    }

    #[test]
    fn asan_emits_module_ctor() {
        let (mut ctx, mut module) = make_load_module();
        let mut pass = Asan::default();
        pass.run_on_module(&mut ctx, &mut module);
        assert!(
            module.get_function_id("__asan_module_ctor").is_some(),
            "__asan_module_ctor must be added"
        );
    }

    #[test]
    fn asan_instruments_load_adds_instructions() {
        let (mut ctx, mut module) = make_load_module();
        // Count instructions in f before instrumentation.
        let before = module.functions[0].instructions.len();

        let mut pass = Asan::default();
        pass.run_on_module(&mut ctx, &mut module);

        let after = module.functions[0].instructions.len();
        assert!(
            after > before,
            "load instrumentation must add instructions ({before} → {after})"
        );
    }

    #[test]
    fn asan_instruments_store_adds_instructions() {
        let (mut ctx, mut module) = make_store_module();
        let before = module.functions[0].instructions.len();

        let mut pass = Asan::default();
        pass.run_on_module(&mut ctx, &mut module);

        let after = module.functions[0].instructions.len();
        assert!(
            after > before,
            "store instrumentation must add instructions ({before} → {after})"
        );
    }

    #[test]
    fn asan_adds_stack_redzone_alloca() {
        let (mut ctx, mut module) = make_alloca_module();

        let count_alloca = |module: &Module| -> usize {
            module.functions[0]
                .blocks
                .iter()
                .flat_map(|bb| bb.body.iter())
                .filter(|&&iid| {
                    matches!(
                        module.functions[0].instr(iid).kind,
                        InstrKind::Alloca { .. }
                    )
                })
                .count()
        };

        let before = count_alloca(&module);
        assert_eq!(before, 1, "sanity check: one alloca before pass");

        let mut pass = Asan::default();
        pass.run_on_module(&mut ctx, &mut module);

        let after = count_alloca(&module);
        assert!(
            after > before,
            "stack redzone allocas must have been inserted ({before} → {after})"
        );
    }

    #[test]
    fn asan_skips_declarations() {
        let mut ctx = Context::new();
        let fn_ty = ctx.mk_fn_type(ctx.void_ty, vec![], false);
        let decl = Function::new_declaration("ext", fn_ty, vec![], Linkage::External);
        let mut module = Module::new("test");
        module.add_function(decl);

        // One declaration; record its instruction count.
        let before = module.functions[0].instructions.len();

        let mut pass = Asan::default();
        pass.run_on_module(&mut ctx, &mut module);

        let after = module.functions[0].instructions.len();
        assert_eq!(
            before, after,
            "declaration function must not be instrumented"
        );
    }

    #[test]
    fn asan_is_idempotent_on_ctor() {
        let (mut ctx, mut module) = make_load_module();
        let mut pass = Asan::default();
        pass.run_on_module(&mut ctx, &mut module);
        let ctor_count_1 = module
            .functions
            .iter()
            .filter(|f| f.name == "__asan_module_ctor")
            .count();
        pass.run_on_module(&mut ctx, &mut module);
        let ctor_count_2 = module
            .functions
            .iter()
            .filter(|f| f.name == "__asan_module_ctor")
            .count();
        assert_eq!(ctor_count_1, ctor_count_2, "module ctor must not be duplicated");
    }
}
