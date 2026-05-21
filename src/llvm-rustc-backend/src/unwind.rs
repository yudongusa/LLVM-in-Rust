//! Rust panic/unwinding support for the rustc codegen backend.
//!
//! Provides helpers to:
//! - Emit `invoke` instead of `call` for potentially-panicking functions
//! - Create cleanup landingpad blocks with rust_eh_personality
//! - Emit LSDA call-site records for `.gcc_except_table`
//! - Handle `resume` terminator
//!
//! # Overview
//!
//! When a Rust function may panic the compiler replaces `call f(args)` with:
//!
//! ```text
//! invoke f(args) to label %normal unwind label %cleanup
//! cleanup:
//!   landingpad { i8*, i32 } personality i8* bitcast (...) @rust_eh_personality cleanup
//!   br %handler
//! handler:
//!   ...
//!   resume { %exn_ptr, %sel }
//! ```
//!
//! The backend additionally emits an LSDA (Language Specific Data Area) in
//! `.gcc_except_table` that maps each `invoke` call-site to its landing pad.
//!
//! # Windows SEH
//!
//! Windows SEH uses `__CxxFrameHandler3` and a completely different table
//! format.  That is intentionally out of scope here.
//! Windows: TODO

use llvm_ir::{
    basic_block::BasicBlock,
    context::{BlockId, FunctionId, ValueRef},
    function::Function,
    instruction::{InstrKind, Instruction, LandingPadClause},
    module::Module,
    value::{Argument, Linkage},
    Context,
};

// ---------------------------------------------------------------------------
// Personality function
// ---------------------------------------------------------------------------

/// Declare `rust_eh_personality` as an external function in the module.
///
/// Uses the Itanium C++ ABI EH personality signature:
/// ```text
/// declare i32 @rust_eh_personality(i32, i64, i8*, i8*)
/// ```
///
/// Calling this function twice with the same module is idempotent: the second
/// call returns the existing `FunctionId` without inserting a duplicate.
pub fn declare_personality_fn(ctx: &mut Context, module: &mut Module) -> FunctionId {
    const NAME: &str = "rust_eh_personality";

    // Return existing declaration if already present.
    if let Some(id) = module.get_function_id(NAME) {
        return id;
    }

    // Itanium EH personality signature:
    //   i32 @rust_eh_personality(i32 version, i64 actions, i8* exception_class, i8* ue_header)
    let i32_ty = ctx.i32_ty;
    let i64_ty = ctx.i64_ty;
    let ptr_ty = ctx.ptr_ty;
    let fn_ty = ctx.mk_fn_type(i32_ty, vec![i32_ty, i64_ty, ptr_ty, ptr_ty], false);

    let args = vec![
        Argument { name: String::new(), ty: i32_ty, index: 0 },
        Argument { name: String::new(), ty: i64_ty, index: 1 },
        Argument { name: String::new(), ty: ptr_ty, index: 2 },
        Argument { name: String::new(), ty: ptr_ty, index: 3 },
    ];
    let decl = llvm_ir::function::Function::new_declaration(NAME, fn_ty, args, Linkage::External);
    module.add_function(decl)
}

// ---------------------------------------------------------------------------
// Landing-pad block
// ---------------------------------------------------------------------------

/// Emit a landing-pad basic block that:
/// 1. Extracts the exception value via a `landingpad` instruction
/// 2. Branches unconditionally to `cleanup_dest` for further cleanup
///
/// The block structure emitted is:
/// ```text
/// <name>:
///   %lp = landingpad { i8*, i32 } personality i8* bitcast (...) @<personality> cleanup
///   br label %cleanup_dest
/// ```
///
/// Returns the `BlockId` of the newly created landing-pad block.
pub fn emit_landingpad_block(
    ctx: &mut Context,
    func: &mut Function,
    personality: FunctionId,
    cleanup_dest: BlockId,
) -> BlockId {
    // Build the `{ i8*, i32 }` struct type that the landingpad produces.
    let ptr_ty = ctx.ptr_ty;
    let i32_ty = ctx.i32_ty;
    let lp_ty = ctx.mk_struct_anon(vec![ptr_ty, i32_ty], false);

    // The personality function value: Global(id) — by the project's convention
    // GlobalId(i) == FunctionId(i), so we use GlobalId to form the ValueRef.
    let personality_ref = ValueRef::Global(llvm_ir::context::GlobalId(personality.0));

    // landingpad instruction
    let lp_instr = Instruction::new(
        Some("lp".to_string()),
        lp_ty,
        InstrKind::LandingPad {
            result_ty: lp_ty,
            personality_fn: Some(personality_ref),
            cleanup: true,
            clauses: Vec::<LandingPadClause>::new(),
        },
    );

    // Branch to cleanup_dest
    let br_instr = Instruction::new(
        None,
        ctx.void_ty,
        InstrKind::Br { dest: cleanup_dest },
    );

    let mut bb = BasicBlock::new("landingpad");
    let lp_id = func.alloc_instr(lp_instr);
    let br_id = func.alloc_instr(br_instr);
    bb.append_instr(lp_id);
    bb.set_terminator(br_id);
    func.add_block(bb)
}

// ---------------------------------------------------------------------------
// LSDA builder
// ---------------------------------------------------------------------------

/// A single call-site record in the LSDA call-site table.
///
/// Each record describes one `invoke` instruction and maps it to a landing
/// pad (or indicates "no handler" if `lp_offset == 0`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallSiteRecord {
    /// Byte offset of the start of the `invoke` call in `.text`.
    pub cs_start: u32,
    /// Length in bytes of the call instruction sequence.
    pub cs_len: u32,
    /// Byte offset of the landing pad in `.text`.  0 means "no handler".
    pub lp_offset: u32,
    /// Index into the action table.  0 means "cleanup only" (no typed catch).
    pub action: u32,
}

/// Accumulates call-site records and encodes them as a GCC LSDA
/// (`.gcc_except_table` section).
///
/// The LSDA format used here is the GCC/Itanium ABI "Language Specific Data
/// Area" format, used by libunwind on Linux and macOS:
///
/// ```text
/// header:
///   u8  lpstart_encoding   ; DW_EH_PE_omit (0xff) — lpstart == func start
///   u8  ttype_encoding     ; DW_EH_PE_omit (0xff) — no type table
///   u8  call_site_encoding ; DW_EH_PE_uleb128 (0x01)
///   uleb128 call_site_table_length
/// call-site table:
///   for each record:
///     uleb128 cs_start
///     uleb128 cs_len
///     uleb128 lp_offset
///     uleb128 action
/// ```
pub struct LsdaBuilder {
    /// Accumulated call-site records.
    pub call_sites: Vec<CallSiteRecord>,
}

impl LsdaBuilder {
    /// Create an empty builder.
    pub fn new() -> Self {
        LsdaBuilder { call_sites: Vec::new() }
    }

    /// Append one call-site record.
    pub fn add_call_site(&mut self, record: CallSiteRecord) {
        self.call_sites.push(record);
    }

    /// Encode the LSDA to bytes in GCC exception-table format.
    ///
    /// The result is suitable for placement in the `.gcc_except_table`
    /// object-file section.
    pub fn encode(&self) -> Vec<u8> {
        // Encode all call-site records first so we know their total length.
        let mut cs_bytes: Vec<u8> = Vec::new();
        for rec in &self.call_sites {
            cs_bytes.extend(encode_uleb128(rec.cs_start as u64));
            cs_bytes.extend(encode_uleb128(rec.cs_len as u64));
            cs_bytes.extend(encode_uleb128(rec.lp_offset as u64));
            cs_bytes.extend(encode_uleb128(rec.action as u64));
        }

        let mut out: Vec<u8> = Vec::new();

        // lpstart encoding: DW_EH_PE_omit (0xff)
        // Means the landing-pad base == function start (no explicit lpstart).
        out.push(0xff);

        // ttype encoding: DW_EH_PE_omit (0xff)
        // No type-filter table (cleanup-only).
        out.push(0xff);

        // call-site encoding: DW_EH_PE_uleb128 (0x01)
        out.push(0x01);

        // call-site table length as ULEB128
        out.extend(encode_uleb128(cs_bytes.len() as u64));

        // call-site records
        out.extend(cs_bytes);

        out
    }
}

impl Default for LsdaBuilder {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// ULEB128 helper
// ---------------------------------------------------------------------------

/// Encode `val` as an unsigned LEB128 byte sequence.
///
/// Used for all variable-length integers in the LSDA call-site table.
pub fn encode_uleb128(mut val: u64) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let byte = (val & 0x7f) as u8;
        val >>= 7;
        if val == 0 {
            out.push(byte); // high bit clear — last byte
            break;
        } else {
            out.push(byte | 0x80); // high bit set — more bytes follow
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Tests (stable only, no rustc_private)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use llvm_ir::{Context, Module};

    // ── helper ────────────────────────────────────────────────────────────────

    fn mk_ctx_module() -> (Context, Module) {
        (Context::new(), Module::new("test"))
    }

    fn mk_func(ctx: &mut Context, module: &mut Module) -> FunctionId {
        use llvm_ir::value::Linkage;
        let fn_ty = ctx.mk_fn_type(ctx.void_ty, vec![], false);
        let func = llvm_ir::function::Function::new("test_fn", fn_ty, vec![], Linkage::External);
        module.add_function(func)
    }

    // ── 1. personality_fn_is_declared ────────────────────────────────────────

    #[test]
    fn personality_fn_is_declared() {
        let (mut ctx, mut module) = mk_ctx_module();
        let _fid = declare_personality_fn(&mut ctx, &mut module);
        assert!(
            module.get_function_id("rust_eh_personality").is_some(),
            "rust_eh_personality must be added to the module"
        );
    }

    // ── 2. personality_fn_is_idempotent ──────────────────────────────────────

    #[test]
    fn personality_fn_is_idempotent() {
        let (mut ctx, mut module) = mk_ctx_module();
        let id1 = declare_personality_fn(&mut ctx, &mut module);
        let id2 = declare_personality_fn(&mut ctx, &mut module);
        assert_eq!(id1, id2, "double declaration must return the same FunctionId");
        assert_eq!(module.num_functions(), 1, "only one function must be in the module");
    }

    // ── 3. landingpad_block_exists ────────────────────────────────────────────

    #[test]
    fn landingpad_block_exists() {
        let (mut ctx, mut module) = mk_ctx_module();
        let personality = declare_personality_fn(&mut ctx, &mut module);
        let fid = mk_func(&mut ctx, &mut module);
        let func = module.function_mut(fid);

        // Add a cleanup_dest block for the branch target.
        let cleanup = func.add_block(BasicBlock::new("cleanup"));

        let lp_block = emit_landingpad_block(&mut ctx, func, personality, cleanup);

        // The block id must be valid (less than the total block count).
        assert!(
            (lp_block.0 as usize) < func.num_blocks(),
            "landing pad BlockId must index a valid block"
        );
    }

    // ── 4. landingpad_block_has_landingpad_instr ──────────────────────────────

    #[test]
    fn landingpad_block_has_landingpad_instr() {
        let (mut ctx, mut module) = mk_ctx_module();
        let personality = declare_personality_fn(&mut ctx, &mut module);
        let fid = mk_func(&mut ctx, &mut module);
        let func = module.function_mut(fid);

        let cleanup = func.add_block(BasicBlock::new("cleanup"));
        let lp_bid = emit_landingpad_block(&mut ctx, func, personality, cleanup);

        let bb = func.block(lp_bid);
        assert!(
            !bb.body.is_empty(),
            "landingpad block must have at least one body instruction"
        );
        let first_iid = bb.body[0];
        let first_instr = func.instr(first_iid);
        assert!(
            matches!(first_instr.kind, InstrKind::LandingPad { cleanup: true, .. }),
            "first instruction in landing-pad block must be LandingPad{{cleanup:true}}, got {:?}",
            first_instr.kind,
        );
    }

    // ── 5. lsda_encode_single_callsite ───────────────────────────────────────

    #[test]
    fn lsda_encode_single_callsite() {
        let mut builder = LsdaBuilder::new();
        builder.add_call_site(CallSiteRecord {
            cs_start: 10,
            cs_len: 5,
            lp_offset: 20,
            action: 0,
        });
        let bytes = builder.encode();

        // Header: lpstart=0xff, ttype=0xff, cs_encoding=0x01
        assert_eq!(bytes[0], 0xff, "lpstart_encoding must be DW_EH_PE_omit");
        assert_eq!(bytes[1], 0xff, "ttype_encoding must be DW_EH_PE_omit");
        assert_eq!(bytes[2], 0x01, "call_site_encoding must be DW_EH_PE_uleb128");

        // After the header and the table-length uleb128, the record should
        // encode cs_start=10, cs_len=5, lp_offset=20, action=0.
        // All values < 128, so each is a single byte.
        // table_length = 4 bytes (one byte per field) → uleb128 = [0x04]
        assert_eq!(bytes[3], 4, "call-site table length must be 4");
        assert_eq!(bytes[4], 10, "cs_start");
        assert_eq!(bytes[5], 5, "cs_len");
        assert_eq!(bytes[6], 20, "lp_offset");
        assert_eq!(bytes[7], 0, "action");
        assert_eq!(bytes.len(), 8);
    }

    // ── 6. lsda_encode_empty ─────────────────────────────────────────────────

    #[test]
    fn lsda_encode_empty() {
        let builder = LsdaBuilder::new();
        let bytes = builder.encode();
        // Header: 3 bytes + uleb128(0) = 1 byte = 4 bytes total.
        assert_eq!(bytes[0], 0xff, "lpstart_encoding");
        assert_eq!(bytes[1], 0xff, "ttype_encoding");
        assert_eq!(bytes[2], 0x01, "call_site_encoding");
        assert_eq!(bytes[3], 0x00, "empty table length");
        assert_eq!(bytes.len(), 4);
    }

    // ── 7. uleb128_encoding ───────────────────────────────────────────────────

    #[test]
    fn uleb128_encoding() {
        // 0 → [0x00]
        assert_eq!(encode_uleb128(0), vec![0x00]);
        // 1 → [0x01]
        assert_eq!(encode_uleb128(1), vec![0x01]);
        // 127 → [0x7f]  (max single-byte value)
        assert_eq!(encode_uleb128(127), vec![0x7f]);
        // 128 → [0x80, 0x01]  (first two-byte value)
        assert_eq!(encode_uleb128(128), vec![0x80, 0x01]);
        // 300 = 0b1_0010_1100 → low 7 bits = 0b010_1100 = 44, next = 0b10 = 2
        //   → [0b1_010_1100, 0b000_0010] = [0xac, 0x02]
        assert_eq!(encode_uleb128(300), vec![0xac, 0x02]);
    }
}
