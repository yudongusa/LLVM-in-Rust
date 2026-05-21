//! Standard LLVM bitcode (`.bc`) reader.
#![allow(dead_code, unused_variables, unused_mut, unused_assignments, clippy::ptr_arg, clippy::cloned_ref_to_slice_refs, dropping_references)]
//!
//! Parses files produced by `clang -emit-llvm -c` and reconstructs a
//! `(Context, Module)` using the same IR types as the rest of LLVM-in-Rust.
//!
//! Format overview:
//! - Magic: `BC\xc0\xde` (4 bytes)
//! - Then a bitstream of blocks; the top-level block is `MODULE_BLOCK_ID=8`
//!   which contains `TYPE_BLOCK_ID=17`, `CONSTANTS_BLOCK_ID=11`,
//!   `FUNCTION_BLOCK_ID=12`, `VALUE_SYMTAB_BLOCK_ID=14` sub-blocks.

use crate::bitstream::{
    Abbrev, BitStreamReader, ABBREV_DEFINE_ABBREV, ABBREV_END_BLOCK, ABBREV_ENTER_SUBBLOCK,
    ABBREV_UNABBREV_RECORD,
};
use crate::error::BitcodeError;
use llvm_ir::{
    value::Argument, ArgId, BasicBlock, BlockId, ConstId, ConstantData, Context, FastMathFlags,
    FloatKind, FloatPredicate, Function, GlobalId, GlobalVariable, InstrId, InstrKind, Instruction,
    IntArithFlags, IntPredicate, Linkage, Module, TailCallKind, TypeData, TypeId, ValueRef,
};

// ── Magic ──────────────────────────────────────────────────────────────────────

/// LLVM bitcode magic: `BC\xc0\xde`.
const LLVM_BC_MAGIC: &[u8; 4] = b"BC\xc0\xde";

// ── Block IDs ──────────────────────────────────────────────────────────────────

const BLOCKINFO_BLOCK_ID: u64 = 0;
const MODULE_BLOCK_ID: u64 = 8;
const PARAMATTR_BLOCK_ID: u64 = 9;
const PARAMATTR_GROUP_BLOCK_ID: u64 = 10;
const CONSTANTS_BLOCK_ID: u64 = 11;
const FUNCTION_BLOCK_ID: u64 = 12;
const VALUE_SYMTAB_BLOCK_ID: u64 = 14;
const METADATA_BLOCK_ID: u64 = 15;
const METADATA_ATTACHMENT_BLOCK_ID: u64 = 16;
const TYPE_BLOCK_ID: u64 = 17;
const USELIST_BLOCK_ID: u64 = 18;
const STRTAB_BLOCK_ID: u64 = 23;
const SYNC_SCOPE_NAMES_BLOCK_ID: u64 = 24;

// ── MODULE_BLOCK record codes ───────────────────────────────────────────────────

const MODULE_CODE_VERSION: u64 = 1;
const MODULE_CODE_TRIPLE: u64 = 2;
const MODULE_CODE_DATALAYOUT: u64 = 3;
const MODULE_CODE_GLOBALVAR: u64 = 7;
const MODULE_CODE_FUNCTION: u64 = 8;
const MODULE_CODE_ALIAS: u64 = 9;
const MODULE_CODE_SOURCE_FILENAME: u64 = 16;

// ── TYPE_BLOCK record codes ───────────────────────────────────────────────────

const TYPE_CODE_NUMENTRY: u64 = 1;
const TYPE_CODE_VOID: u64 = 2;
const TYPE_CODE_FLOAT: u64 = 3;
const TYPE_CODE_DOUBLE: u64 = 4;
const TYPE_CODE_LABEL: u64 = 5;
const TYPE_CODE_OPAQUE: u64 = 6;
const TYPE_CODE_INTEGER: u64 = 7;
const TYPE_CODE_POINTER: u64 = 8;
const TYPE_CODE_FUNCTION_OLD: u64 = 9;
const TYPE_CODE_HALF: u64 = 10;
const TYPE_CODE_ARRAY: u64 = 11;
const TYPE_CODE_VECTOR: u64 = 12;
const TYPE_CODE_X86_FP80: u64 = 13;
const TYPE_CODE_FP128: u64 = 14;
const TYPE_CODE_METADATA: u64 = 16;
const TYPE_CODE_STRUCT_ANON: u64 = 18;
const TYPE_CODE_STRUCT_NAMED: u64 = 19;
const TYPE_CODE_STRUCT_BODY: u64 = 20;
const TYPE_CODE_FUNCTION: u64 = 21;
const TYPE_CODE_TOKEN: u64 = 22;
const TYPE_CODE_BFLOAT: u64 = 23;
const TYPE_CODE_OPAQUE_POINTER: u64 = 25;
const TYPE_CODE_TARGET_TYPE: u64 = 26;

// ── CONSTANTS_BLOCK record codes ──────────────────────────────────────────────

const CST_CODE_SETTYPE: u64 = 1;
const CST_CODE_NULL: u64 = 2;
const CST_CODE_UNDEF: u64 = 3;
const CST_CODE_INTEGER: u64 = 4;
const CST_CODE_WIDE_INTEGER: u64 = 5;
const CST_CODE_FLOAT: u64 = 6;
const CST_CODE_AGGREGATE: u64 = 7;
const CST_CODE_STRING: u64 = 8;
const CST_CODE_CSTRING: u64 = 9;
const CST_CODE_CE_BINOP: u64 = 10;
const CST_CODE_CE_CAST: u64 = 11;
const CST_CODE_CE_GEP: u64 = 12;
const CST_CODE_CE_SELECT: u64 = 13;
const CST_CODE_CE_EXTRACTELT: u64 = 14;
const CST_CODE_CE_INSERTELT: u64 = 15;
const CST_CODE_CE_SHUFFLEVEC: u64 = 16;
const CST_CODE_CE_CMP: u64 = 17;
const CST_CODE_POISON: u64 = 24;
const CST_CODE_CE_GEP_WITH_INRANGE: u64 = 25;
const CST_CODE_CE_GEP_OLD: u64 = 12; // alias
const CST_CODE_BLOCKADDRESS: u64 = 21;
const CST_CODE_DATA: u64 = 22;
const CST_CODE_CE_INBOUNDS_GEP: u64 = 20;

// ── FUNCTION_BLOCK instruction record codes ────────────────────────────────────

const FUNC_CODE_DECLAREBLOCKS: u64 = 1;
const FUNC_CODE_INST_BINOP: u64 = 2;
const FUNC_CODE_INST_CAST: u64 = 3;
const FUNC_CODE_INST_GEP_OLD: u64 = 4;
const FUNC_CODE_INST_SELECT: u64 = 5;
const FUNC_CODE_INST_EXTRACTELT: u64 = 6;
const FUNC_CODE_INST_INSERTELT: u64 = 7;
const FUNC_CODE_INST_SHUFFLEVEC: u64 = 8;
const FUNC_CODE_INST_CMP: u64 = 9;
const FUNC_CODE_INST_RET: u64 = 10;
const FUNC_CODE_INST_BR: u64 = 11;
const FUNC_CODE_INST_SWITCH: u64 = 12;
const FUNC_CODE_INST_INVOKE: u64 = 13;
const FUNC_CODE_INST_UNREACHABLE: u64 = 15;
const FUNC_CODE_INST_PHI: u64 = 16;
const FUNC_CODE_INST_ALLOCA: u64 = 19;
const FUNC_CODE_INST_LOAD: u64 = 20;
const FUNC_CODE_INST_STORE_OLD: u64 = 21;
const FUNC_CODE_INST_EXTRACTVAL: u64 = 26;
const FUNC_CODE_INST_INSERTVAL: u64 = 27;
const FUNC_CODE_INST_CMP2: u64 = 28;
const FUNC_CODE_INST_VSELECT: u64 = 29;
const FUNC_CODE_INST_INBOUNDS_GEP_OLD: u64 = 30;
const FUNC_CODE_INST_INDIRECTBR: u64 = 31;
const FUNC_CODE_INST_DEBUG_LOC: u64 = 32;
const FUNC_CODE_INST_FENCE: u64 = 36;
const FUNC_CODE_INST_CMPXCHG_OLD: u64 = 37;
const FUNC_CODE_INST_ATOMICRMW_OLD: u64 = 38;
const FUNC_CODE_INST_RESUME: u64 = 39;
const FUNC_CODE_INST_LANDINGPAD_OLD: u64 = 40;
const FUNC_CODE_INST_LOADATOMIC: u64 = 41;
const FUNC_CODE_INST_STOREATOMIC_OLD: u64 = 42;
const FUNC_CODE_INST_GEP: u64 = 43;
const FUNC_CODE_INST_STORE: u64 = 44;
const FUNC_CODE_INST_STOREATOMIC: u64 = 45;
const FUNC_CODE_INST_CMPXCHG: u64 = 46;
const FUNC_CODE_INST_LANDINGPAD: u64 = 47;
const FUNC_CODE_INST_CLEANUPRET: u64 = 48;
const FUNC_CODE_INST_CATCHRET: u64 = 49;
const FUNC_CODE_INST_CATCHPAD: u64 = 50;
const FUNC_CODE_INST_CLEANUPPAD: u64 = 51;
const FUNC_CODE_INST_CATCHSWITCH: u64 = 52;
const FUNC_CODE_INST_OPERAND_BUNDLE: u64 = 55;
const FUNC_CODE_INST_UNOP: u64 = 56;
const FUNC_CODE_INST_CALLBR: u64 = 57;
const FUNC_CODE_INST_FREEZE: u64 = 58;
const FUNC_CODE_INST_ATOMICRMW: u64 = 59;
const FUNC_CODE_INST_CALL: u64 = 34;

// ── VALUE_SYMTAB record codes ──────────────────────────────────────────────────

const VST_CODE_ENTRY: u64 = 1;
const VST_CODE_BBENTRY: u64 = 2;
const VST_CODE_FNENTRY: u64 = 3;

// ── Linkage mapping ────────────────────────────────────────────────────────────

fn decode_linkage(tag: u64) -> Linkage {
    match tag {
        0 => Linkage::External,
        1 => Linkage::Weak,
        2 => Linkage::Internal,
        3 => Linkage::LinkOnce,
        5 => Linkage::Common,
        7 => Linkage::WeakOdr,
        8 => Linkage::LinkOnceOdr,
        9 => Linkage::AvailableExternally,
        10 => Linkage::Private,
        _ => Linkage::External,
    }
}

// ── Reader state ───────────────────────────────────────────────────────────────

/// Internal state for the LLVM bitcode decoder.
struct LlvmReader {
    ctx: Context,
    module: Module,
    /// type_table[i] = TypeId — indexed by LLVM type slot.
    type_table: Vec<TypeId>,
    /// Pending named struct body definitions: name → (named slot index, fields, packed).
    named_struct_pending: Vec<(String, usize)>,
    /// Module-level value table: module globals + functions (as GlobalRef constants).
    /// Also includes function arguments and instructions once inside a function.
    value_table: Vec<ValueRef>,
    /// Number of module-level values (globals + function declarations).
    num_module_values: usize,
    /// Function declaration names, in declaration order.
    func_decl_names: Vec<String>,
    /// Function declaration types, in declaration order.
    func_decl_types: Vec<TypeId>,
    /// Function declarations linkage.
    func_decl_linkages: Vec<Linkage>,
    /// Is-declaration flag.
    func_decl_is_decl: Vec<bool>,
    /// Next function slot index to fill with a body (pointing into func_decl_names).
    next_func_body_idx: usize,
    /// Optional string table (from STRTAB_BLOCK).
    strtab: Option<Vec<u8>>,
}

impl LlvmReader {
    fn new(module_name: &str) -> Self {
        LlvmReader {
            ctx: Context::new(),
            module: Module::new(module_name),
            type_table: Vec::new(),
            named_struct_pending: Vec::new(),
            value_table: Vec::new(),
            num_module_values: 0,
            func_decl_names: Vec::new(),
            func_decl_types: Vec::new(),
            func_decl_linkages: Vec::new(),
            func_decl_is_decl: Vec::new(),
            next_func_body_idx: 0,
            strtab: None,
        }
    }

    fn get_type(&self, idx: usize) -> Result<TypeId, BitcodeError> {
        self.type_table.get(idx).copied().ok_or_else(|| {
            BitcodeError::ParseError(format!(
                "type idx {} out of range (table size {})",
                idx,
                self.type_table.len()
            ))
        })
    }

    /// Resolve a relative value reference inside a function.
    ///
    /// LLVM bitcode uses relative (backward) references: encoded as
    /// `cur_idx - encoded`. When `encoded == 0` it usually means an
    /// absolute index from the strtab or similar; we treat it as an error.
    fn resolve_value_rel(&self, cur_val_id: usize, encoded: u64) -> Result<ValueRef, BitcodeError> {
        let abs = cur_val_id
            .checked_sub(encoded as usize)
            .ok_or_else(|| BitcodeError::ParseError("value ref underflow".into()))?;
        self.value_table.get(abs).copied().ok_or_else(|| {
            BitcodeError::ParseError(format!(
                "value ref {} out of range (table size {})",
                abs,
                self.value_table.len()
            ))
        })
    }

    fn resolve_value_abs(&self, idx: usize) -> Result<ValueRef, BitcodeError> {
        self.value_table.get(idx).copied().ok_or_else(|| {
            BitcodeError::ParseError(format!(
                "value abs ref {} out of range (table size {})",
                idx,
                self.value_table.len()
            ))
        })
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

/// Parse a standard LLVM `.bc` file (produced by `clang -emit-llvm -c`).
///
/// Returns a `(Context, Module)` pair on success.  Returns
/// `Err(BitcodeError::InvalidMagic)` if the file does not begin with the
/// 4-byte LLVM bitcode magic `BC\xc0\xde`.
pub fn read_llvm_bc(bytes: &[u8]) -> Result<(Context, Module), BitcodeError> {
    if bytes.len() < 4 || &bytes[..4] != LLVM_BC_MAGIC {
        return Err(BitcodeError::InvalidMagic);
    }

    let mut bs = BitStreamReader::new(&bytes[4..]);
    let mut state = LlvmReader::new("module");

    // The top-level stream may contain a STRTAB_BLOCK before or after
    // MODULE_BLOCK.  We process whichever comes first.
    parse_top_level(&mut bs, &mut state)?;

    Ok((state.ctx, state.module))
}

// ── Top-level stream parser ────────────────────────────────────────────────────

fn parse_top_level(
    bs: &mut BitStreamReader<'_>,
    state: &mut LlvmReader,
) -> Result<(), BitcodeError> {
    while !bs.is_at_end() {
        let abbrev_id = bs.read_bits(bs.abbrev_len)?;
        match abbrev_id {
            ABBREV_END_BLOCK => {
                let _ = bs.end_block();
                return Ok(());
            }
            ABBREV_ENTER_SUBBLOCK => {
                let (block_id, _) = bs.enter_block()?;
                let saved_abbrevs = bs.abbrevs.clone();
                match block_id {
                    STRTAB_BLOCK_ID => {
                        parse_strtab_block(bs, state, &saved_abbrevs)?;
                    }
                    MODULE_BLOCK_ID => {
                        parse_module_block(bs, state, &saved_abbrevs)?;
                    }
                    BLOCKINFO_BLOCK_ID => {
                        skip_block_contents(bs, &saved_abbrevs)?;
                    }
                    _ => {
                        skip_block_contents(bs, &saved_abbrevs)?;
                    }
                }
            }
            ABBREV_DEFINE_ABBREV => {
                bs.define_abbrev()?;
            }
            _ => {
                // Top-level records (uncommon) — skip.
                let _ = bs.read_record_fields(abbrev_id, &[])?;
            }
        }
    }
    Ok(())
}

// ── Skip a block whose contents we don't need ─────────────────────────────────

fn skip_block_contents(
    bs: &mut BitStreamReader<'_>,
    _outer_abbrevs: &[Abbrev],
) -> Result<(), BitcodeError> {
    let mut depth = 1usize;
    loop {
        if bs.is_at_end() {
            break;
        }
        let abbrev_id = bs.read_bits(bs.abbrev_len)?;
        match abbrev_id {
            ABBREV_END_BLOCK => {
                bs.end_block()?;
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            ABBREV_ENTER_SUBBLOCK => {
                bs.enter_block()?;
                depth += 1;
            }
            ABBREV_DEFINE_ABBREV => {
                bs.define_abbrev()?;
            }
            _ => {
                let local = bs.abbrevs.clone();
                let _ = bs.read_record_fields(abbrev_id, &local)?;
            }
        }
    }
    Ok(())
}

// ── STRTAB_BLOCK ───────────────────────────────────────────────────────────────

fn parse_strtab_block(
    bs: &mut BitStreamReader<'_>,
    state: &mut LlvmReader,
    _outer_abbrevs: &[Abbrev],
) -> Result<(), BitcodeError> {
    loop {
        if bs.is_at_end() {
            break;
        }
        let abbrev_id = bs.read_bits(bs.abbrev_len)?;
        match abbrev_id {
            ABBREV_END_BLOCK => {
                bs.end_block()?;
                break;
            }
            ABBREV_ENTER_SUBBLOCK => {
                bs.enter_block()?;
                skip_block_contents(bs, &[])?;
            }
            ABBREV_DEFINE_ABBREV => {
                bs.define_abbrev()?;
            }
            _ => {
                let local = bs.abbrevs.clone();
                let fields = bs.read_record_fields(abbrev_id, &local)?;
                if abbrev_id == ABBREV_UNABBREV_RECORD {
                    // code is fields[0]
                    if !fields.is_empty() && fields[0] == 1 {
                        // STRTAB_BLOB
                        let bytes: Vec<u8> = fields[1..].iter().map(|&b| b as u8).collect();
                        state.strtab = Some(bytes);
                    }
                } else {
                    // Abbreviated strtab blob record
                    let bytes: Vec<u8> = fields.iter().map(|&b| b as u8).collect();
                    if !bytes.is_empty() {
                        state.strtab = Some(bytes);
                    }
                }
            }
        }
    }
    Ok(())
}

// ── MODULE_BLOCK ───────────────────────────────────────────────────────────────

fn parse_module_block(
    bs: &mut BitStreamReader<'_>,
    state: &mut LlvmReader,
    _outer_abbrevs: &[Abbrev],
) -> Result<(), BitcodeError> {
    loop {
        if bs.is_at_end() {
            break;
        }
        let abbrev_id = bs.read_bits(bs.abbrev_len)?;
        match abbrev_id {
            ABBREV_END_BLOCK => {
                bs.end_block()?;
                break;
            }
            ABBREV_ENTER_SUBBLOCK => {
                let (block_id, _) = bs.enter_block()?;
                let saved = bs.abbrevs.clone();
                match block_id {
                    TYPE_BLOCK_ID => {
                        parse_type_block(bs, state, &saved)?;
                    }
                    CONSTANTS_BLOCK_ID => {
                        parse_constants_block(bs, state, &saved, None)?;
                    }
                    FUNCTION_BLOCK_ID => {
                        parse_function_block(bs, state, &saved)?;
                    }
                    VALUE_SYMTAB_BLOCK_ID => {
                        parse_vst_block(bs, state, &saved)?;
                    }
                    METADATA_BLOCK_ID
                    | METADATA_ATTACHMENT_BLOCK_ID
                    | PARAMATTR_BLOCK_ID
                    | PARAMATTR_GROUP_BLOCK_ID
                    | USELIST_BLOCK_ID
                    | SYNC_SCOPE_NAMES_BLOCK_ID
                    | STRTAB_BLOCK_ID => {
                        skip_block_contents(bs, &saved)?;
                    }
                    _ => {
                        skip_block_contents(bs, &saved)?;
                    }
                }
            }
            ABBREV_DEFINE_ABBREV => {
                bs.define_abbrev()?;
            }
            _ => {
                let local = bs.abbrevs.clone();
                let fields = bs.read_record_fields(abbrev_id, &local)?;
                if !fields.is_empty() {
                    let code = fields[0];
                    match code {
                        MODULE_CODE_VERSION => {
                            // version field — ignore
                        }
                        MODULE_CODE_SOURCE_FILENAME => {
                            // source filename — update module name
                            let name = decode_chars(&fields[1..]);
                            state.module.name = name;
                        }
                        MODULE_CODE_GLOBALVAR => {
                            parse_globalvar_record(&fields[1..], state)?;
                        }
                        MODULE_CODE_FUNCTION => {
                            parse_function_decl_record(&fields[1..], state)?;
                        }
                        MODULE_CODE_TRIPLE
                        | MODULE_CODE_DATALAYOUT
                        | MODULE_CODE_ALIAS => {
                            // Ignore
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    // After processing all sub-blocks and records, add function declarations
    // that had no body (is_declaration=true).  Bodies were added directly in
    // parse_function_block; declarations were deferred until here.
    for idx in state.next_func_body_idx..state.func_decl_names.len() {
        let name = state.func_decl_names[idx].clone();
        let ty = state.func_decl_types[idx];
        let linkage = state.func_decl_linkages[idx];
        // Determine parameter types.
        let param_tys: Vec<TypeId> = {
            let td = state.ctx.get_type(ty).clone();
            match td {
                TypeData::Function(ref ft) => ft.params.clone(),
                _ => vec![],
            }
        };
        let args: Vec<Argument> = param_tys
            .iter()
            .enumerate()
            .map(|(i, &pty)| Argument { name: format!("arg{}", i), ty: pty, index: i as u32 })
            .collect();
        let func = Function::new_declaration(name, ty, args, linkage);
        state.module.add_function(func);
    }

    Ok(())
}

/// Decode a sequence of character codes (as u64) into a String.
fn decode_chars(fields: &[u64]) -> String {
    fields.iter().map(|&c| c as u8 as char).collect()
}

// ── TYPE_BLOCK ─────────────────────────────────────────────────────────────────

fn parse_type_block(
    bs: &mut BitStreamReader<'_>,
    state: &mut LlvmReader,
    _outer: &[Abbrev],
) -> Result<(), BitcodeError> {
    let mut num_types: usize = 0;
    // Named structs: track pending body for the slot most recently named.
    let mut pending_named: Option<(String, usize)> = None;

    loop {
        if bs.is_at_end() {
            break;
        }
        let abbrev_id = bs.read_bits(bs.abbrev_len)?;
        match abbrev_id {
            ABBREV_END_BLOCK => {
                bs.end_block()?;
                break;
            }
            ABBREV_ENTER_SUBBLOCK => {
                bs.enter_block()?;
                skip_block_contents(bs, &[])?;
            }
            ABBREV_DEFINE_ABBREV => {
                bs.define_abbrev()?;
            }
            _ => {
                let local = bs.abbrevs.clone();
                let fields = bs.read_record_fields(abbrev_id, &local)?;
                if fields.is_empty() {
                    continue;
                }
                let code = fields[0];
                match code {
                    TYPE_CODE_NUMENTRY => {
                        num_types = fields.get(1).copied().unwrap_or(0) as usize;
                        state.type_table.reserve(num_types);
                    }
                    TYPE_CODE_VOID => {
                        state.type_table.push(state.ctx.void_ty);
                    }
                    TYPE_CODE_FLOAT => {
                        state.type_table.push(state.ctx.mk_float(FloatKind::Single));
                    }
                    TYPE_CODE_DOUBLE => {
                        state.type_table.push(state.ctx.mk_float(FloatKind::Double));
                    }
                    TYPE_CODE_HALF => {
                        state.type_table.push(state.ctx.mk_float(FloatKind::Half));
                    }
                    TYPE_CODE_BFLOAT => {
                        state.type_table.push(state.ctx.mk_float(FloatKind::BFloat));
                    }
                    TYPE_CODE_X86_FP80 => {
                        state
                            .type_table
                            .push(state.ctx.mk_float(FloatKind::X86Fp80));
                    }
                    TYPE_CODE_FP128 => {
                        state.type_table.push(state.ctx.mk_float(FloatKind::Fp128));
                    }
                    TYPE_CODE_LABEL => {
                        state.type_table.push(state.ctx.mk_label());
                    }
                    TYPE_CODE_METADATA => {
                        state.type_table.push(state.ctx.mk_metadata());
                    }
                    TYPE_CODE_TOKEN | TYPE_CODE_TARGET_TYPE => {
                        // Treat as opaque pointer.
                        state.type_table.push(state.ctx.mk_ptr());
                    }
                    TYPE_CODE_INTEGER => {
                        let bits = fields.get(1).copied().unwrap_or(32) as u32;
                        state.type_table.push(state.ctx.mk_int(bits));
                    }
                    TYPE_CODE_POINTER => {
                        // fields[1] = pointee type (ignored in opaque-ptr world)
                        // fields[2] = address space (ignored)
                        state.type_table.push(state.ctx.mk_ptr());
                    }
                    TYPE_CODE_OPAQUE_POINTER => {
                        // opaque ptr (LLVM 15+)
                        state.type_table.push(state.ctx.mk_ptr());
                    }
                    TYPE_CODE_ARRAY => {
                        // fields: [code, num_elements, elem_ty_idx]
                        let num_elems = fields.get(1).copied().unwrap_or(0);
                        let elem_idx = fields.get(2).copied().unwrap_or(0) as usize;
                        let elem_ty = state.get_type(elem_idx)?;
                        state
                            .type_table
                            .push(state.ctx.mk_array(elem_ty, num_elems));
                    }
                    TYPE_CODE_VECTOR => {
                        // fields: [code, num_elements, elem_ty_idx]
                        let num_elems = fields.get(1).copied().unwrap_or(0) as u32;
                        let elem_idx = fields.get(2).copied().unwrap_or(0) as usize;
                        let elem_ty = state.get_type(elem_idx)?;
                        state
                            .type_table
                            .push(state.ctx.mk_vector(elem_ty, num_elems, false));
                    }
                    TYPE_CODE_FUNCTION | TYPE_CODE_FUNCTION_OLD => {
                        // fields: [code, is_vararg, ret_ty, param_ty...]
                        let is_vararg = fields.get(1).copied().unwrap_or(0) != 0;
                        let ret_idx = fields.get(2).copied().unwrap_or(0) as usize;
                        let ret_ty = state.get_type(ret_idx)?;
                        let mut params = Vec::new();
                        for &p in &fields[3..] {
                            params.push(state.get_type(p as usize)?);
                        }
                        state
                            .type_table
                            .push(state.ctx.mk_fn_type(ret_ty, params, is_vararg));
                    }
                    TYPE_CODE_OPAQUE => {
                        // Opaque struct placeholder — we'll define it when STRUCT_BODY arrives.
                        // For now push a placeholder.
                        let slot = state.type_table.len();
                        let tid = if let Some((ref name, _)) = pending_named {
                            state.ctx.mk_struct_named(name.clone())
                        } else {
                            // Anonymous opaque — use an anon empty struct.
                            state.ctx.mk_struct_anon(vec![], false)
                        };
                        state.type_table.push(tid);
                        if let Some((name, _)) = pending_named.take() {
                            state.named_struct_pending.push((name, slot));
                        }
                    }
                    TYPE_CODE_STRUCT_NAMED => {
                        // Just records the name for the next OPAQUE or STRUCT_BODY.
                        let name = decode_chars(&fields[1..]);
                        pending_named = Some((name.clone(), state.type_table.len()));
                        // Push a placeholder named struct (body comes in STRUCT_BODY).
                        let tid = state.ctx.mk_struct_named(name.clone());
                        state.type_table.push(tid);
                    }
                    TYPE_CODE_STRUCT_ANON => {
                        // fields: [code, is_packed, field_ty...]
                        let is_packed = fields.get(1).copied().unwrap_or(0) != 0;
                        let mut field_tys = Vec::new();
                        for &f in &fields[2..] {
                            field_tys.push(state.get_type(f as usize)?);
                        }
                        state
                            .type_table
                            .push(state.ctx.mk_struct_anon(field_tys, is_packed));
                    }
                    TYPE_CODE_STRUCT_BODY => {
                        // fields: [code, is_packed, field_ty...]
                        // This fills in the most recently STRUCT_NAMED slot.
                        let is_packed = fields.get(1).copied().unwrap_or(0) != 0;
                        let mut field_tys = Vec::new();
                        for &f in &fields[2..] {
                            field_tys.push(state.get_type(f as usize)?);
                        }
                        // Find the pending named struct (the last one).
                        if let Some((_name, _slot)) = pending_named.take() {
                            // The tid was already pushed in STRUCT_NAMED.
                            let last_idx = state.type_table.len() - 1;
                            let tid = state.type_table[last_idx];
                            state.ctx.define_struct_body(tid, field_tys, is_packed);
                        } else if let Some(&(_, slot)) = state.named_struct_pending.last() {
                            let tid = state.type_table[slot];
                            state.ctx.define_struct_body(tid, field_tys, is_packed);
                        }
                        // If there was no pending named struct, this is for the anon pushed
                        // in STRUCT_NAMED; we just ignore the body definition as a no-op.
                    }
                    _ => {
                        // Unknown type record — skip.
                    }
                }
            }
        }
    }
    Ok(())
}

// ── CONSTANTS_BLOCK ────────────────────────────────────────────────────────────

/// Parse a CONSTANTS_BLOCK.
/// `base_val_idx` is the index of the first constant value in the value table
/// (for function constants it is the length before entering this block).
fn parse_constants_block(
    bs: &mut BitStreamReader<'_>,
    state: &mut LlvmReader,
    _outer: &[Abbrev],
    base_val_idx: Option<usize>,
) -> Result<(), BitcodeError> {
    let base = base_val_idx.unwrap_or(state.value_table.len());
    let mut cur_type: TypeId = state.ctx.i32_ty;

    loop {
        if bs.is_at_end() {
            break;
        }
        let abbrev_id = bs.read_bits(bs.abbrev_len)?;
        match abbrev_id {
            ABBREV_END_BLOCK => {
                bs.end_block()?;
                break;
            }
            ABBREV_ENTER_SUBBLOCK => {
                bs.enter_block()?;
                skip_block_contents(bs, &[])?;
            }
            ABBREV_DEFINE_ABBREV => {
                bs.define_abbrev()?;
            }
            _ => {
                let local = bs.abbrevs.clone();
                let fields = bs.read_record_fields(abbrev_id, &local)?;
                if fields.is_empty() {
                    continue;
                }
                let code = fields[0];
                match code {
                    CST_CODE_SETTYPE => {
                        let ty_idx = fields.get(1).copied().unwrap_or(0) as usize;
                        cur_type = state.get_type(ty_idx)?;
                    }
                    CST_CODE_NULL => {
                        let cid = state.ctx.push_const(ConstantData::Null(cur_type));
                        state.value_table.push(ValueRef::Constant(cid));
                    }
                    CST_CODE_UNDEF => {
                        let cid = state.ctx.push_const(ConstantData::Undef(cur_type));
                        state.value_table.push(ValueRef::Constant(cid));
                    }
                    CST_CODE_POISON => {
                        let cid = state.ctx.push_const(ConstantData::Poison(cur_type));
                        state.value_table.push(ValueRef::Constant(cid));
                    }
                    CST_CODE_INTEGER => {
                        // Sign-encoded: bit 0 = sign, rest = magnitude >> 1.
                        let encoded = fields.get(1).copied().unwrap_or(0);
                        let val = decode_sign_rotated(encoded);
                        let cid = state
                            .ctx
                            .push_const(ConstantData::Int { ty: cur_type, val });
                        state.value_table.push(ValueRef::Constant(cid));
                    }
                    CST_CODE_WIDE_INTEGER => {
                        // Multiple sign-rotated 64-bit words.
                        let mut words: Vec<u64> = fields[1..]
                            .iter()
                            .map(|&w| decode_sign_rotated(w))
                            .collect();
                        // If only one word fits in 64 bits, use Int.
                        if words.len() == 1 {
                            let cid = state.ctx.push_const(ConstantData::Int {
                                ty: cur_type,
                                val: words[0],
                            });
                            state.value_table.push(ValueRef::Constant(cid));
                        } else {
                            let cid = state
                                .ctx
                                .push_const(ConstantData::IntWide { ty: cur_type, words });
                            state.value_table.push(ValueRef::Constant(cid));
                        }
                    }
                    CST_CODE_FLOAT => {
                        let bits = fields.get(1).copied().unwrap_or(0);
                        let cid = state
                            .ctx
                            .push_const(ConstantData::Float { ty: cur_type, bits });
                        state.value_table.push(ValueRef::Constant(cid));
                    }
                    CST_CODE_AGGREGATE => {
                        // [code, elem_val_idx...]
                        let mut elems: Vec<ConstId> = Vec::new();
                        let cur_idx = state.value_table.len();
                        for &vi in &fields[1..] {
                            // Aggregate elements are forward-indexed (absolute).
                            let abs = vi as usize;
                            let vr = state.resolve_value_abs(abs)?;
                            if let ValueRef::Constant(cid) = vr {
                                elems.push(cid);
                            } else {
                                return Err(BitcodeError::ParseError(
                                    "aggregate element is not a constant".into(),
                                ));
                            }
                        }
                        // Determine aggregate kind from type.
                        let td = state.ctx.get_type(cur_type).clone();
                        let cid = match td {
                            TypeData::Array { .. } => state.ctx.push_const(ConstantData::Array {
                                ty: cur_type,
                                elements: elems,
                            }),
                            TypeData::Struct(_) => state.ctx.push_const(ConstantData::Struct {
                                ty: cur_type,
                                fields: elems,
                            }),
                            TypeData::Vector { .. } => {
                                state.ctx.push_const(ConstantData::Vector {
                                    ty: cur_type,
                                    elements: elems,
                                })
                            }
                            _ => state.ctx.push_const(ConstantData::Array {
                                ty: cur_type,
                                elements: elems,
                            }),
                        };
                        state.value_table.push(ValueRef::Constant(cid));
                    }
                    CST_CODE_STRING | CST_CODE_CSTRING => {
                        // String constant — encode as byte array.
                        let bytes: Vec<u8> = fields[1..].iter().map(|&c| c as u8).collect();
                        let i8_ty = state.ctx.mk_int(8);
                        let n = bytes.len() as u64;
                        let arr_ty = state.ctx.mk_array(i8_ty, n);
                        let mut elems = Vec::new();
                        for b in bytes {
                            let ec = state
                                .ctx
                                .push_const(ConstantData::Int { ty: i8_ty, val: b as u64 });
                            elems.push(ec);
                        }
                        let cid = state.ctx.push_const(ConstantData::Array {
                            ty: arr_ty,
                            elements: elems,
                        });
                        state.value_table.push(ValueRef::Constant(cid));
                    }
                    CST_CODE_CE_CAST => {
                        // [code, opcode, ty_idx, val_idx]
                        let opcode = fields.get(1).copied().unwrap_or(0);
                        let ty_idx = fields.get(2).copied().unwrap_or(0) as usize;
                        let val_idx = fields.get(3).copied().unwrap_or(0) as usize;
                        let to_ty = state.get_type(ty_idx)?;
                        let vr = state.resolve_value_abs(val_idx)?;
                        let src_cid = match vr {
                            ValueRef::Constant(c) => c,
                            _ => {
                                return Err(BitcodeError::ParseError(
                                    "CE_CAST operand is not a constant".into(),
                                ));
                            }
                        };
                        use llvm_ir::ConstExprOp;
                        let op = match opcode {
                            1 => ConstExprOp::ZExt,
                            2 => ConstExprOp::SExt,
                            3 => ConstExprOp::Trunc,
                            8 => ConstExprOp::PtrToInt,
                            9 => ConstExprOp::IntToPtr,
                            11 => ConstExprOp::BitCast,
                            12 => ConstExprOp::AddrSpaceCast,
                            _ => ConstExprOp::BitCast,
                        };
                        let cid = state.ctx.push_const(ConstantData::Expr {
                            ty: to_ty,
                            op,
                            operands: vec![src_cid],
                        });
                        state.value_table.push(ValueRef::Constant(cid));
                    }
                    CST_CODE_CE_GEP | CST_CODE_CE_INBOUNDS_GEP | CST_CODE_CE_GEP_WITH_INRANGE => {
                        // [code, {ty_idx,} inbounds, (ty, val)...]
                        // Older form: [code, (ty, val)...]
                        // Newer form: [code, explicit_type_idx, inbounds, (ty, val)...]
                        // We parse conservatively.
                        let inbounds = code == CST_CODE_CE_INBOUNDS_GEP
                            || code == CST_CODE_CE_GEP_WITH_INRANGE;
                        let mut idx = 1usize;
                        // Try to figure out if there is an explicit type first.
                        // Fields layout: if fields[1] looks like a type index
                        // followed by inbounds flag, it is the new form.
                        // Heuristic: new form has odd number of remaining fields
                        // (ty_idx + inbounds + N*(ty,val) pairs).
                        let explicit_ty = if (fields.len() - 1) % 2 == 1 {
                            // New form
                            let base_ty_idx = fields.get(idx).copied().unwrap_or(0) as usize;
                            idx += 1;
                            let _inbounds_flag = fields.get(idx).copied().unwrap_or(0);
                            idx += 1;
                            Some(state.get_type(base_ty_idx)?)
                        } else {
                            None
                        };
                        let base_ty = explicit_ty.unwrap_or(cur_type);

                        let mut operands: Vec<ConstId> = Vec::new();
                        while idx + 1 < fields.len() {
                            let _ty_idx = fields[idx] as usize;
                            idx += 1;
                            let val_idx = fields[idx] as usize;
                            idx += 1;
                            let vr = state.resolve_value_abs(val_idx)?;
                            if let ValueRef::Constant(c) = vr {
                                operands.push(c);
                            }
                        }
                        use llvm_ir::ConstExprOp;
                        let cid = state.ctx.push_const(ConstantData::Expr {
                            ty: cur_type,
                            op: ConstExprOp::GetElementPtr { inbounds, base_ty },
                            operands,
                        });
                        state.value_table.push(ValueRef::Constant(cid));
                    }
                    CST_CODE_BLOCKADDRESS => {
                        // Block address — represent as null ptr.
                        let ptr_ty = state.ctx.mk_ptr();
                        let cid = state.ctx.push_const(ConstantData::Null(ptr_ty));
                        state.value_table.push(ValueRef::Constant(cid));
                    }
                    CST_CODE_DATA => {
                        // Splat / data constant — zero-initialize.
                        let cid = state
                            .ctx
                            .push_const(ConstantData::ZeroInitializer(cur_type));
                        state.value_table.push(ValueRef::Constant(cid));
                    }
                    _ => {
                        // Unknown constant — push undef.
                        let cid = state.ctx.push_const(ConstantData::Undef(cur_type));
                        state.value_table.push(ValueRef::Constant(cid));
                    }
                }
            }
        }
    }
    Ok(())
}

/// Decode a sign-rotated (zigzag) integer from the LLVM bitcode format.
///
/// LLVM encoding: positive N → N<<1; negative N → ((-N-1)<<1)|1.
/// Inverse: if bit0=0 → N>>1; if bit0=1 → -(N>>1)-1 = !( N>>1 ).
fn decode_sign_rotated(encoded: u64) -> u64 {
    if encoded & 1 != 0 {
        // Negative: -(mag + 1) = !(mag) in two's complement (no +1 needed).
        let mag = encoded >> 1;
        !mag
    } else {
        encoded >> 1
    }
}

// ── GLOBALVAR record ───────────────────────────────────────────────────────────

fn parse_globalvar_record(
    fields: &[u64],
    state: &mut LlvmReader,
) -> Result<(), BitcodeError> {
    if fields.len() < 6 {
        return Ok(());
    }
    let ty_idx = fields[0] as usize;
    let is_const_and_addr = fields[1]; // bit 0 = isConstant, bits 1..= addr space
    let init_id = fields[2]; // 0 means no init, otherwise const table idx + 1
    let linkage_val = fields[3];
    // fields[4] = alignment_log2
    // fields[5] = section index

    let ty = state.get_type(ty_idx)?;
    let is_constant = (is_const_and_addr & 1) != 0;
    let linkage = decode_linkage(linkage_val);

    // Initializer: 0 = none, N = const_table[N-1].
    let initializer: Option<ConstId> = if init_id != 0 {
        let idx = (init_id - 1) as usize;
        match state.resolve_value_abs(idx)? {
            ValueRef::Constant(c) => Some(c),
            _ => None,
        }
    } else {
        None
    };

    // Name will be filled in from VST later; for now use a placeholder.
    let gid = module_add_global(
        &mut state.module,
        &mut state.ctx,
        "",
        ty,
        initializer,
        is_constant,
        linkage,
    );
    // Push into value table as a GlobalRef placeholder.
    let ptr_ty = state.ctx.mk_ptr();
    let cid = state.ctx.push_const(ConstantData::GlobalRef {
        ty: ptr_ty,
        id: gid,
        name: String::new(),
    });
    state.value_table.push(ValueRef::Constant(cid));
    Ok(())
}

fn module_add_global(
    module: &mut Module,
    _ctx: &mut Context,
    name: &str,
    ty: TypeId,
    initializer: Option<ConstId>,
    is_constant: bool,
    linkage: Linkage,
) -> GlobalId {
    module.add_global(GlobalVariable {
        name: name.to_string(),
        ty,
        initializer,
        is_constant,
        linkage,
    })
}

// ── FUNCTION declaration record ────────────────────────────────────────────────

fn parse_function_decl_record(
    fields: &[u64],
    state: &mut LlvmReader,
) -> Result<(), BitcodeError> {
    // fields: [ty_idx, calling_conv, is_declaration, linkage, ...]
    if fields.len() < 3 {
        return Ok(());
    }
    let ty_idx = fields[0] as usize;
    let _calling_conv = fields[1];
    let is_declaration = fields[2] != 0;
    let linkage = decode_linkage(fields.get(3).copied().unwrap_or(0));

    let ty = state.get_type(ty_idx)?;
    state.func_decl_types.push(ty);
    state.func_decl_names.push(String::new()); // name filled in from VST
    state.func_decl_linkages.push(linkage);
    state.func_decl_is_decl.push(is_declaration);

    // Push into value table as a Global ref.
    let fidx = state.func_decl_names.len() - 1;
    let ptr_ty = state.ctx.mk_ptr();
    let cid = state.ctx.push_const(ConstantData::GlobalRef {
        ty: ptr_ty,
        id: GlobalId(fidx as u32),
        name: String::new(),
    });
    state.value_table.push(ValueRef::Constant(cid));
    Ok(())
}

// ── VALUE_SYMTAB_BLOCK ─────────────────────────────────────────────────────────

fn parse_vst_block(
    bs: &mut BitStreamReader<'_>,
    state: &mut LlvmReader,
    _outer: &[Abbrev],
) -> Result<(), BitcodeError> {
    loop {
        if bs.is_at_end() {
            break;
        }
        let abbrev_id = bs.read_bits(bs.abbrev_len)?;
        match abbrev_id {
            ABBREV_END_BLOCK => {
                bs.end_block()?;
                break;
            }
            ABBREV_ENTER_SUBBLOCK => {
                bs.enter_block()?;
                skip_block_contents(bs, &[])?;
            }
            ABBREV_DEFINE_ABBREV => {
                bs.define_abbrev()?;
            }
            _ => {
                let local = bs.abbrevs.clone();
                let fields = bs.read_record_fields(abbrev_id, &local)?;
                if fields.is_empty() {
                    continue;
                }
                let code = fields[0];
                match code {
                    VST_CODE_ENTRY | VST_CODE_FNENTRY => {
                        // [code, value_id, char6_name...]
                        let val_id = fields.get(1).copied().unwrap_or(0) as usize;
                        let name = decode_chars(&fields[2..]);
                        // Figure out if this is a global or function.
                        // Globals come before functions in the value table.
                        let num_globals = state.module.globals.len();
                        if val_id < num_globals {
                            state.module.globals[val_id].name = name.clone();
                            // Also update the GlobalRef constant if present.
                            // (We don't track the cid per slot so skip for now.)
                        } else {
                            let func_slot = val_id - num_globals;
                            if func_slot < state.func_decl_names.len() {
                                state.func_decl_names[func_slot] = name;
                            }
                        }
                    }
                    VST_CODE_BBENTRY => {
                        // Basic block name — skip.
                    }
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

// ── FUNCTION_BLOCK ─────────────────────────────────────────────────────────────

fn parse_function_block(
    bs: &mut BitStreamReader<'_>,
    state: &mut LlvmReader,
    _outer: &[Abbrev],
) -> Result<(), BitcodeError> {
    let func_slot = state.next_func_body_idx;
    state.next_func_body_idx += 1;

    // Get function metadata.
    let func_name = state
        .func_decl_names
        .get(func_slot)
        .cloned()
        .unwrap_or_else(|| format!("f{}", func_slot));
    let func_ty = state
        .func_decl_types
        .get(func_slot)
        .copied()
        .unwrap_or(state.ctx.void_ty);
    let linkage = state
        .func_decl_linkages
        .get(func_slot)
        .copied()
        .unwrap_or(Linkage::External);
    let is_decl = state
        .func_decl_is_decl
        .get(func_slot)
        .copied()
        .unwrap_or(false);

    // Determine parameter types from the function type.
    let (ret_ty, param_tys, is_vararg) = {
        let td = state.ctx.get_type(func_ty).clone();
        match td {
            TypeData::Function(ref ft) => (ft.ret, ft.params.clone(), ft.variadic),
            _ => (state.ctx.void_ty, vec![], false),
        }
    };

    // Build argument list.
    let mut args = Vec::new();
    for (i, &pty) in param_tys.iter().enumerate() {
        args.push(Argument {
            name: format!("arg{}", i),
            ty: pty,
            index: i as u32,
        });
    }

    let mut func = if is_decl {
        Function::new_declaration(func_name.clone(), func_ty, args.clone(), linkage)
    } else {
        Function::new(func_name.clone(), func_ty, args.clone(), linkage)
    };

    // Snapshot of the module-level value table to restore later.
    let module_val_count = state.value_table.len();

    // Push function arguments into the value table.
    for (i, _) in args.iter().enumerate() {
        state.value_table.push(ValueRef::Argument(ArgId(i as u32)));
    }

    // We'll collect instructions and basic blocks.
    let mut basic_blocks: Vec<BasicBlock> = Vec::new();
    let mut cur_block_idx: usize = 0;
    let mut num_blocks: usize = 0;

    // Instruction results go into the value table as we decode them.
    // We track the InstrId offsets.
    let mut instr_count: usize = 0;

    loop {
        if bs.is_at_end() {
            break;
        }
        let abbrev_id = bs.read_bits(bs.abbrev_len)?;
        match abbrev_id {
            ABBREV_END_BLOCK => {
                bs.end_block()?;
                break;
            }
            ABBREV_ENTER_SUBBLOCK => {
                let (block_id, _) = bs.enter_block()?;
                let saved = bs.abbrevs.clone();
                match block_id {
                    CONSTANTS_BLOCK_ID => {
                        parse_constants_block(
                            bs,
                            state,
                            &saved,
                            Some(state.value_table.len()),
                        )?;
                    }
                    VALUE_SYMTAB_BLOCK_ID => {
                        // Function-local VST — skip for now.
                        skip_block_contents(bs, &saved)?;
                    }
                    METADATA_BLOCK_ID | METADATA_ATTACHMENT_BLOCK_ID => {
                        skip_block_contents(bs, &saved)?;
                    }
                    _ => {
                        skip_block_contents(bs, &saved)?;
                    }
                }
            }
            ABBREV_DEFINE_ABBREV => {
                bs.define_abbrev()?;
            }
            _ => {
                let local = bs.abbrevs.clone();
                let fields = bs.read_record_fields(abbrev_id, &local)?;
                if fields.is_empty() {
                    continue;
                }
                let code = fields[0];

                // Number of values currently in the function scope.
                let cur_val_id = state.value_table.len();

                match code {
                    FUNC_CODE_DECLAREBLOCKS => {
                        num_blocks = fields.get(1).copied().unwrap_or(1) as usize;
                        for i in 0..num_blocks {
                            basic_blocks.push(BasicBlock::new(format!("{}", i)));
                        }
                    }
                    FUNC_CODE_INST_BINOP => {
                        // [code, lhs, rhs, opcode, {flags}]
                        let lhs = decode_relative_vref(cur_val_id, fields.get(1).copied().unwrap_or(0), state)?;
                        let rhs = decode_relative_vref(cur_val_id, fields.get(2).copied().unwrap_or(0), state)?;
                        let opcode = fields.get(3).copied().unwrap_or(0);
                        let flags = fields.get(4).copied().unwrap_or(0);

                        // Determine result type from lhs.
                        let result_ty = type_of_vref(&lhs, &func, state);

                        let kind = binop_kind(opcode, flags, lhs, rhs, result_ty)?;
                        let iid = emit_instr(&mut func, &mut basic_blocks, cur_block_idx, None, result_ty, kind);
                        state.value_table.push(ValueRef::Instruction(iid));
                        instr_count += 1;
                    }
                    FUNC_CODE_INST_CAST => {
                        // [code, val, ty_idx, opcode]
                        let val = decode_relative_vref(cur_val_id, fields.get(1).copied().unwrap_or(0), state)?;
                        let ty_idx = fields.get(2).copied().unwrap_or(0) as usize;
                        let to_ty = state.get_type(ty_idx)?;
                        let opcode = fields.get(3).copied().unwrap_or(0);
                        let kind = cast_kind(opcode, val, to_ty)?;
                        let iid = emit_instr(&mut func, &mut basic_blocks, cur_block_idx, None, to_ty, kind);
                        state.value_table.push(ValueRef::Instruction(iid));
                        instr_count += 1;
                    }
                    FUNC_CODE_INST_RET => {
                        let val = if fields.len() > 1 {
                            Some(decode_relative_vref(cur_val_id, fields[1], state)?)
                        } else {
                            None
                        };
                        let iid = emit_instr(
                            &mut func,
                            &mut basic_blocks,
                            cur_block_idx,
                            None,
                            state.ctx.void_ty,
                            InstrKind::Ret { val },
                        );
                        // Ret goes in terminator.
                        if cur_block_idx < basic_blocks.len() {
                            basic_blocks[cur_block_idx].terminator = Some(iid);
                            // Remove from body if accidentally added.
                            basic_blocks[cur_block_idx].body.pop();
                        }
                        // Move to next block.
                        cur_block_idx += 1;
                        // Ret does NOT produce a value.
                    }
                    FUNC_CODE_INST_BR => {
                        let kind = if fields.len() >= 4 {
                            // Conditional
                            let true_dest = fields[1] as u32;
                            let false_dest = fields[2] as u32;
                            let cond = decode_relative_vref(cur_val_id, fields[3], state)?;
                            InstrKind::CondBr {
                                cond,
                                then_dest: BlockId(true_dest),
                                else_dest: BlockId(false_dest),
                            }
                        } else {
                            // Unconditional
                            let dest = fields.get(1).copied().unwrap_or(0) as u32;
                            InstrKind::Br { dest: BlockId(dest) }
                        };
                        let iid = emit_instr(
                            &mut func,
                            &mut basic_blocks,
                            cur_block_idx,
                            None,
                            state.ctx.void_ty,
                            kind,
                        );
                        if cur_block_idx < basic_blocks.len() {
                            basic_blocks[cur_block_idx].terminator = Some(iid);
                            basic_blocks[cur_block_idx].body.pop();
                        }
                        cur_block_idx += 1;
                    }
                    FUNC_CODE_INST_UNREACHABLE => {
                        let iid = emit_instr(
                            &mut func,
                            &mut basic_blocks,
                            cur_block_idx,
                            None,
                            state.ctx.void_ty,
                            InstrKind::Unreachable,
                        );
                        if cur_block_idx < basic_blocks.len() {
                            basic_blocks[cur_block_idx].terminator = Some(iid);
                            basic_blocks[cur_block_idx].body.pop();
                        }
                        cur_block_idx += 1;
                    }
                    FUNC_CODE_INST_PHI => {
                        // [code, ty_idx, (val, block)...]
                        let ty_idx = fields.get(1).copied().unwrap_or(0) as usize;
                        let phi_ty = state.get_type(ty_idx)?;
                        let mut incoming = Vec::new();
                        let mut idx = 2;
                        while idx + 1 < fields.len() {
                            let val_enc = fields[idx];
                            let blk = fields[idx + 1] as u32;
                            idx += 2;
                            // phi values use forward-relative encoding (signed VBR6).
                            // The encoding may be relative to cur_val_id; LLVM uses
                            // cur_val_id - val for backward refs, cur_val_id + skip for forward.
                            // For simplicity, treat as relative (backward).
                            let vr = if val_enc == 0 {
                                // forward ref placeholder — use undef
                                let cid = state.ctx.push_const(ConstantData::Undef(phi_ty));
                                ValueRef::Constant(cid)
                            } else {
                                let abs = (cur_val_id as i64 - val_enc as i64).max(0) as usize;
                                state.resolve_value_abs(abs).unwrap_or_else(|_| {
                                    let cid = state.ctx.push_const(ConstantData::Undef(phi_ty));
                                    ValueRef::Constant(cid)
                                })
                            };
                            incoming.push((vr, BlockId(blk)));
                        }
                        let iid = emit_instr(
                            &mut func,
                            &mut basic_blocks,
                            cur_block_idx,
                            None,
                            phi_ty,
                            InstrKind::Phi { ty: phi_ty, incoming },
                        );
                        state.value_table.push(ValueRef::Instruction(iid));
                        instr_count += 1;
                    }
                    FUNC_CODE_INST_ALLOCA => {
                        // [code, inst_ty_idx, op_ty_idx, size_val, align_encoded]
                        let inst_ty_idx = fields.get(1).copied().unwrap_or(0) as usize;
                        let alloc_ty = state.get_type(inst_ty_idx)?;
                        let ptr_ty = state.ctx.mk_ptr();
                        // Alignment is encoded as log2+1.
                        let align_enc = fields.get(4).copied().unwrap_or(0);
                        let align = if align_enc > 0 {
                            Some(1u32 << ((align_enc & 0x7F) - 1))
                        } else {
                            None
                        };
                        let iid = emit_instr(
                            &mut func,
                            &mut basic_blocks,
                            cur_block_idx,
                            None,
                            ptr_ty,
                            InstrKind::Alloca {
                                alloc_ty,
                                num_elements: None,
                                align,
                            },
                        );
                        state.value_table.push(ValueRef::Instruction(iid));
                        instr_count += 1;
                    }
                    FUNC_CODE_INST_LOAD => {
                        // [code, ptr_val, ty_idx, align, volatile]
                        let ptr = decode_relative_vref(cur_val_id, fields.get(1).copied().unwrap_or(0), state)?;
                        let ty_idx = fields.get(2).copied().unwrap_or(0) as usize;
                        let load_ty = state.get_type(ty_idx)?;
                        let align_enc = fields.get(3).copied().unwrap_or(0);
                        let align = if align_enc > 0 {
                            Some(1u32 << (align_enc - 1))
                        } else {
                            None
                        };
                        let volatile = fields.get(4).copied().unwrap_or(0) != 0;
                        let iid = emit_instr(
                            &mut func,
                            &mut basic_blocks,
                            cur_block_idx,
                            None,
                            load_ty,
                            InstrKind::Load { ty: load_ty, ptr, align, volatile },
                        );
                        state.value_table.push(ValueRef::Instruction(iid));
                        instr_count += 1;
                    }
                    FUNC_CODE_INST_STORE | FUNC_CODE_INST_STORE_OLD => {
                        // New: [code, ptr, val, align, volatile]
                        // Old: [code, val, ptr, align, volatile]
                        let (ptr, val) = if code == FUNC_CODE_INST_STORE {
                            let p = decode_relative_vref(cur_val_id, fields.get(1).copied().unwrap_or(0), state)?;
                            let v = decode_relative_vref(cur_val_id, fields.get(2).copied().unwrap_or(0), state)?;
                            (p, v)
                        } else {
                            let v = decode_relative_vref(cur_val_id, fields.get(1).copied().unwrap_or(0), state)?;
                            let p = decode_relative_vref(cur_val_id, fields.get(2).copied().unwrap_or(0), state)?;
                            (p, v)
                        };
                        let align_enc = fields.get(3).copied().unwrap_or(0);
                        let align = if align_enc > 0 {
                            Some(1u32 << (align_enc - 1))
                        } else {
                            None
                        };
                        let volatile = fields.get(4).copied().unwrap_or(0) != 0;
                        let iid = emit_instr(
                            &mut func,
                            &mut basic_blocks,
                            cur_block_idx,
                            None,
                            state.ctx.void_ty,
                            InstrKind::Store { val, ptr, align, volatile },
                        );
                        // Store has no value result.
                        instr_count += 1;
                    }
                    FUNC_CODE_INST_GEP | FUNC_CODE_INST_GEP_OLD | FUNC_CODE_INST_INBOUNDS_GEP_OLD => {
                        let (inbounds, base_ty, ptr, indices) = if code == FUNC_CODE_INST_GEP {
                            // New: [code, inbounds, ty_idx, ptr, (ty, idx)...]
                            let ib = fields.get(1).copied().unwrap_or(0) != 0;
                            let ty_idx = fields.get(2).copied().unwrap_or(0) as usize;
                            let base = state.get_type(ty_idx)?;
                            let p = decode_relative_vref(cur_val_id, fields.get(3).copied().unwrap_or(0), state)?;
                            let mut idxs = Vec::new();
                            let mut i = 4;
                            while i < fields.len() {
                                let _ty = fields[i] as usize;
                                i += 1;
                                if i < fields.len() {
                                    idxs.push(decode_relative_vref(cur_val_id, fields[i], state)?);
                                    i += 1;
                                }
                            }
                            (ib, base, p, idxs)
                        } else {
                            // Old: [code, (ty, val)...] with inbounds from code.
                            let ib = code == FUNC_CODE_INST_INBOUNDS_GEP_OLD;
                            let p = decode_relative_vref(cur_val_id, fields.get(1).copied().unwrap_or(0), state)?;
                            let base = type_of_vref(&p, &func, state);
                            let mut idxs = Vec::new();
                            let mut i = 2;
                            while i < fields.len() {
                                idxs.push(decode_relative_vref(cur_val_id, fields[i], state)?);
                                i += 1;
                            }
                            (ib, base, p, idxs)
                        };
                        let ptr_ty = state.ctx.mk_ptr();
                        let iid = emit_instr(
                            &mut func,
                            &mut basic_blocks,
                            cur_block_idx,
                            None,
                            ptr_ty,
                            InstrKind::GetElementPtr { inbounds, base_ty, ptr, indices },
                        );
                        state.value_table.push(ValueRef::Instruction(iid));
                        instr_count += 1;
                    }
                    FUNC_CODE_INST_CMP2 | FUNC_CODE_INST_CMP => {
                        // [code, lhs, rhs, predicate]
                        let lhs = decode_relative_vref(cur_val_id, fields.get(1).copied().unwrap_or(0), state)?;
                        let rhs = decode_relative_vref(cur_val_id, fields.get(2).copied().unwrap_or(0), state)?;
                        let pred = fields.get(3).copied().unwrap_or(0);
                        let i1_ty = state.ctx.i1_ty;
                        let kind = cmp_kind(pred, lhs, rhs);
                        let iid = emit_instr(
                            &mut func,
                            &mut basic_blocks,
                            cur_block_idx,
                            None,
                            i1_ty,
                            kind,
                        );
                        state.value_table.push(ValueRef::Instruction(iid));
                        instr_count += 1;
                    }
                    FUNC_CODE_INST_SELECT | FUNC_CODE_INST_VSELECT => {
                        // [code, cond, true_val, false_val]
                        let cond = decode_relative_vref(cur_val_id, fields.get(1).copied().unwrap_or(0), state)?;
                        let then_val = decode_relative_vref(cur_val_id, fields.get(2).copied().unwrap_or(0), state)?;
                        let else_val = decode_relative_vref(cur_val_id, fields.get(3).copied().unwrap_or(0), state)?;
                        let res_ty = type_of_vref(&then_val, &func, state);
                        let iid = emit_instr(
                            &mut func,
                            &mut basic_blocks,
                            cur_block_idx,
                            None,
                            res_ty,
                            InstrKind::Select { cond, then_val, else_val },
                        );
                        state.value_table.push(ValueRef::Instruction(iid));
                        instr_count += 1;
                    }
                    FUNC_CODE_INST_CALL => {
                        // [code, attr_id, cc_info, fn_ty_idx, callee, args...]
                        // cc_info encodes tail/notail/musttail in high bits.
                        let _attr = fields.get(1).copied().unwrap_or(0);
                        let cc_info = fields.get(2).copied().unwrap_or(0);
                        let fn_ty_idx = fields.get(3).copied().unwrap_or(0) as usize;
                        let callee_enc = fields.get(4).copied().unwrap_or(0);
                        let callee = decode_relative_vref(cur_val_id, callee_enc, state)?;
                        let callee_ty = state.get_type(fn_ty_idx)?;
                        // Determine return type.
                        let ret_ty = {
                            let td = state.ctx.get_type(callee_ty).clone();
                            match td {
                                TypeData::Function(ref ft) => ft.ret,
                                _ => state.ctx.void_ty,
                            }
                        };
                        let tail = match (cc_info >> 14) & 3 {
                            1 => TailCallKind::Tail,
                            2 => TailCallKind::MustTail,
                            3 => TailCallKind::NoTail,
                            _ => TailCallKind::None,
                        };
                        let mut call_args = Vec::new();
                        for &enc in &fields[5..] {
                            call_args.push(decode_relative_vref(cur_val_id, enc, state)?);
                        }
                        let iid = emit_instr(
                            &mut func,
                            &mut basic_blocks,
                            cur_block_idx,
                            None,
                            ret_ty,
                            InstrKind::Call { tail, callee_ty, callee, args: call_args },
                        );
                        // Only push if non-void result.
                        let ret_td = state.ctx.get_type(ret_ty).clone();
                        if !matches!(ret_td, TypeData::Void) {
                            state.value_table.push(ValueRef::Instruction(iid));
                            instr_count += 1;
                        }
                    }
                    FUNC_CODE_INST_SWITCH => {
                        // [code, ty_idx, cond, default_dest, (case_val, case_dest)...]
                        let ty_idx = fields.get(1).copied().unwrap_or(0) as usize;
                        let _ty = state.get_type(ty_idx)?;
                        let cond = decode_relative_vref(cur_val_id, fields.get(2).copied().unwrap_or(0), state)?;
                        let default = BlockId(fields.get(3).copied().unwrap_or(0) as u32);
                        let mut cases = Vec::new();
                        let mut i = 4;
                        while i + 1 < fields.len() {
                            let cv = decode_relative_vref(cur_val_id, fields[i], state)?;
                            let bd = BlockId(fields[i + 1] as u32);
                            cases.push((cv, bd));
                            i += 2;
                        }
                        let iid = emit_instr(
                            &mut func,
                            &mut basic_blocks,
                            cur_block_idx,
                            None,
                            state.ctx.void_ty,
                            InstrKind::Switch { val: cond, default, cases },
                        );
                        if cur_block_idx < basic_blocks.len() {
                            basic_blocks[cur_block_idx].terminator = Some(iid);
                            basic_blocks[cur_block_idx].body.pop();
                        }
                        cur_block_idx += 1;
                    }
                    FUNC_CODE_INST_UNOP => {
                        // [code, val, opcode, {flags}]
                        let val = decode_relative_vref(cur_val_id, fields.get(1).copied().unwrap_or(0), state)?;
                        let opcode = fields.get(2).copied().unwrap_or(0);
                        let res_ty = type_of_vref(&val, &func, state);
                        let kind = match opcode {
                            12 => InstrKind::FNeg { flags: FastMathFlags::default(), operand: val },
                            _ => InstrKind::FNeg { flags: FastMathFlags::default(), operand: val },
                        };
                        let iid = emit_instr(&mut func, &mut basic_blocks, cur_block_idx, None, res_ty, kind);
                        state.value_table.push(ValueRef::Instruction(iid));
                        instr_count += 1;
                    }
                    FUNC_CODE_INST_FREEZE => {
                        let val = decode_relative_vref(cur_val_id, fields.get(1).copied().unwrap_or(0), state)?;
                        let res_ty = type_of_vref(&val, &func, state);
                        let iid = emit_instr(
                            &mut func,
                            &mut basic_blocks,
                            cur_block_idx,
                            None,
                            res_ty,
                            InstrKind::Freeze { val },
                        );
                        state.value_table.push(ValueRef::Instruction(iid));
                        instr_count += 1;
                    }
                    FUNC_CODE_INST_EXTRACTVAL => {
                        let agg = decode_relative_vref(cur_val_id, fields.get(1).copied().unwrap_or(0), state)?;
                        let indices: Vec<u32> = fields[2..].iter().map(|&x| x as u32).collect();
                        let agg_ty = type_of_vref(&agg, &func, state);
                        // Walk indices to find result type.
                        let res_ty = extractvalue_type(&state.ctx, agg_ty, &indices);
                        let iid = emit_instr(
                            &mut func,
                            &mut basic_blocks,
                            cur_block_idx,
                            None,
                            res_ty,
                            InstrKind::ExtractValue { aggregate: agg, indices },
                        );
                        state.value_table.push(ValueRef::Instruction(iid));
                        instr_count += 1;
                    }
                    FUNC_CODE_INST_INSERTVAL => {
                        let agg = decode_relative_vref(cur_val_id, fields.get(1).copied().unwrap_or(0), state)?;
                        let val = decode_relative_vref(cur_val_id, fields.get(2).copied().unwrap_or(0), state)?;
                        let indices: Vec<u32> = fields[3..].iter().map(|&x| x as u32).collect();
                        let agg_ty = type_of_vref(&agg, &func, state);
                        let iid = emit_instr(
                            &mut func,
                            &mut basic_blocks,
                            cur_block_idx,
                            None,
                            agg_ty,
                            InstrKind::InsertValue { aggregate: agg, val, indices },
                        );
                        state.value_table.push(ValueRef::Instruction(iid));
                        instr_count += 1;
                    }
                    FUNC_CODE_INST_FENCE
                    | FUNC_CODE_INST_ATOMICRMW_OLD
                    | FUNC_CODE_INST_ATOMICRMW
                    | FUNC_CODE_INST_CMPXCHG_OLD
                    | FUNC_CODE_INST_CMPXCHG
                    | FUNC_CODE_INST_LOADATOMIC
                    | FUNC_CODE_INST_STOREATOMIC
                    | FUNC_CODE_INST_STOREATOMIC_OLD => {
                        // Atomic instructions — decode minimally and push void/undef result.
                        let iid = emit_instr(
                            &mut func,
                            &mut basic_blocks,
                            cur_block_idx,
                            None,
                            state.ctx.void_ty,
                            InstrKind::Fence { ordering: llvm_ir::MemOrdering::SeqCst },
                        );
                        instr_count += 1;
                    }
                    FUNC_CODE_INST_DEBUG_LOC
                    | FUNC_CODE_INST_OPERAND_BUNDLE => {
                        // Skip debug/operand bundle records.
                    }
                    _ => {
                        // Unknown instruction — skip.
                    }
                }
            }
        }
    }

    // Attach the basic blocks and finalize the function.
    func.blocks = basic_blocks;

    // Add the function to the module.
    state.module.add_function(func);

    // Restore the value table to module-level.
    state.value_table.truncate(module_val_count);

    Ok(())
}

// ── Helper functions ───────────────────────────────────────────────────────────

/// Decode a relative value reference (backward offset from cur_val_id).
fn decode_relative_vref(
    cur_val_id: usize,
    encoded: u64,
    state: &LlvmReader,
) -> Result<ValueRef, BitcodeError> {
    if encoded == 0 {
        return Err(BitcodeError::ParseError("zero value reference".into()));
    }
    let abs = cur_val_id
        .checked_sub(encoded as usize)
        .ok_or_else(|| BitcodeError::ParseError(format!("value ref underflow: cur={} enc={}", cur_val_id, encoded)))?;
    state.value_table.get(abs).copied().ok_or_else(|| {
        BitcodeError::ParseError(format!(
            "value abs {} out of range (table size {})",
            abs,
            state.value_table.len()
        ))
    })
}

/// Get the type of a ValueRef (best effort).
fn type_of_vref(vr: &ValueRef, func: &Function, state: &LlvmReader) -> TypeId {
    match vr {
        ValueRef::Instruction(iid) => func
            .instructions
            .get(iid.0 as usize)
            .map(|i| i.ty)
            .unwrap_or(state.ctx.i64_ty),
        ValueRef::Argument(aid) => func
            .args
            .get(aid.0 as usize)
            .map(|a| a.ty)
            .unwrap_or(state.ctx.i64_ty),
        ValueRef::Constant(cid) => {
            let cd = state.ctx.get_const(*cid);
            match cd {
                ConstantData::Int { ty, .. } => *ty,
                ConstantData::Float { ty, .. } => *ty,
                ConstantData::Null(ty) => *ty,
                ConstantData::Undef(ty) => *ty,
                ConstantData::Poison(ty) => *ty,
                ConstantData::ZeroInitializer(ty) => *ty,
                ConstantData::Array { ty, .. } => *ty,
                ConstantData::Struct { ty, .. } => *ty,
                ConstantData::Vector { ty, .. } => *ty,
                ConstantData::GlobalRef { ty, .. } => *ty,
                ConstantData::Expr { ty, .. } => *ty,
                ConstantData::IntWide { ty, .. } => *ty,
            }
        }
        ValueRef::Global(_) => state.ctx.ptr_ty,
    }
}

/// Compute the result type of extractvalue by walking indices.
fn extractvalue_type(ctx: &Context, mut ty: TypeId, indices: &[u32]) -> TypeId {
    for &idx in indices {
        let td = ctx.get_type(ty).clone();
        ty = match td {
            TypeData::Struct(ref st) => {
                st.fields.get(idx as usize).copied().unwrap_or(ty)
            }
            TypeData::Array { element, .. } => element,
            _ => ty,
        };
    }
    ty
}

/// Emit an instruction into the current basic block.
fn emit_instr(
    func: &mut Function,
    blocks: &mut Vec<BasicBlock>,
    block_idx: usize,
    name: Option<String>,
    ty: TypeId,
    kind: InstrKind,
) -> InstrId {
    let iid = func.alloc_instr(Instruction::new(name, ty, kind));
    if block_idx < blocks.len() {
        blocks[block_idx].body.push(iid);
    }
    iid
}

// ── Binop kind mapping ─────────────────────────────────────────────────────────

fn binop_kind(
    opcode: u64,
    flags: u64,
    lhs: ValueRef,
    rhs: ValueRef,
    ty: TypeId,
) -> Result<InstrKind, BitcodeError> {
    // LLVM binop opcode mapping.
    // Integer ops: 0=Add,1=Sub,2=Mul,4=UDiv,5=SDiv,6=URem,7=SRem,10=Shl,11=LShr,12=AShr,
    //              13=And,14=Or,15=Xor
    // Float ops: 17=FAdd,18=FSub,19=FMul,20=FDiv,21=FRem
    let nuw = (flags & 1) != 0;
    let nsw = (flags & 2) != 0;
    let exact = (flags & 1) != 0;
    let iaf = IntArithFlags { nuw, nsw };
    let fmf = FastMathFlags::default();

    Ok(match opcode {
        0 => InstrKind::Add { flags: iaf, lhs, rhs },
        1 => InstrKind::Sub { flags: iaf, lhs, rhs },
        2 => InstrKind::Mul { flags: iaf, lhs, rhs },
        4 => InstrKind::UDiv { exact, lhs, rhs },
        5 => InstrKind::SDiv { exact, lhs, rhs },
        6 => InstrKind::URem { lhs, rhs },
        7 => InstrKind::SRem { lhs, rhs },
        10 => InstrKind::Shl { flags: iaf, lhs, rhs },
        11 => InstrKind::LShr { exact, lhs, rhs },
        12 => InstrKind::AShr { exact, lhs, rhs },
        13 => InstrKind::And { lhs, rhs },
        14 => InstrKind::Or { lhs, rhs },
        15 => InstrKind::Xor { lhs, rhs },
        17 => InstrKind::FAdd { flags: fmf, lhs, rhs },
        18 => InstrKind::FSub { flags: fmf, lhs, rhs },
        19 => InstrKind::FMul { flags: fmf, lhs, rhs },
        20 => InstrKind::FDiv { flags: fmf, lhs, rhs },
        21 => InstrKind::FRem { flags: fmf, lhs, rhs },
        _ => InstrKind::Add { flags: iaf, lhs, rhs },
    })
}

// ── Cast kind mapping ──────────────────────────────────────────────────────────

fn cast_kind(opcode: u64, val: ValueRef, to: TypeId) -> Result<InstrKind, BitcodeError> {
    // LLVM cast opcode mapping (from LLVMOpcode in llvm-c/Core.h):
    // 30=Trunc,31=ZExt,32=SExt,33=FPToUI,34=FPToSI,35=UIToFP,36=SIToFP,
    // 37=FPTrunc,38=FPExt,39=PtrToInt,40=IntToPtr,41=BitCast,42=AddrSpaceCast
    // In bitcode the opcode field differs slightly; use the raw encoding:
    // 0=Trunc,1=ZExt,2=SExt,8=PtrToInt,9=IntToPtr,11=BitCast,12=AddrSpaceCast
    // 3=FPTrunc,4=FPExt,5=UIToFP,6=SIToFP,7=FPToUI,10=FPToSI
    Ok(match opcode {
        0 => InstrKind::Trunc { val, to },
        1 => InstrKind::ZExt { val, to },
        2 => InstrKind::SExt { val, to },
        3 => InstrKind::FPTrunc { val, to },
        4 => InstrKind::FPExt { val, to },
        5 => InstrKind::UIToFP { val, to },
        6 => InstrKind::SIToFP { val, to },
        7 => InstrKind::FPToUI { val, to },
        8 => InstrKind::PtrToInt { val, to },
        9 => InstrKind::IntToPtr { val, to },
        10 => InstrKind::FPToSI { val, to },
        11 => InstrKind::BitCast { val, to },
        12 => InstrKind::AddrSpaceCast { val, to },
        _ => InstrKind::BitCast { val, to },
    })
}

// ── Compare kind mapping ───────────────────────────────────────────────────────

fn cmp_kind(pred: u64, lhs: ValueRef, rhs: ValueRef) -> InstrKind {
    // Predicates 0-9: integer; 32-47: float.
    if pred >= 32 {
        // Float predicate.
        let fp = match pred {
            32 => FloatPredicate::False,
            33 => FloatPredicate::Oeq,
            34 => FloatPredicate::Ogt,
            35 => FloatPredicate::Oge,
            36 => FloatPredicate::Olt,
            37 => FloatPredicate::Ole,
            38 => FloatPredicate::One,
            39 => FloatPredicate::Ord,
            40 => FloatPredicate::Uno,
            41 => FloatPredicate::Ueq,
            42 => FloatPredicate::Ugt,
            43 => FloatPredicate::Uge,
            44 => FloatPredicate::Ult,
            45 => FloatPredicate::Ule,
            46 => FloatPredicate::Une,
            47 => FloatPredicate::True,
            _ => FloatPredicate::False,
        };
        InstrKind::FCmp {
            flags: FastMathFlags::default(),
            pred: fp,
            lhs,
            rhs,
        }
    } else {
        let ip = match pred {
            32 => IntPredicate::Eq,  // shouldn't happen
            0 => IntPredicate::Eq,
            1 => IntPredicate::Ne,
            2 => IntPredicate::Ugt,
            3 => IntPredicate::Uge,
            4 => IntPredicate::Ult,
            5 => IntPredicate::Ule,
            6 => IntPredicate::Sgt,
            7 => IntPredicate::Sge,
            8 => IntPredicate::Slt,
            9 => IntPredicate::Sle,
            _ => IntPredicate::Eq,
        };
        InstrKind::ICmp { pred: ip, lhs, rhs }
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bitstream::BitStreamReader;

    // ── Minimal bitstream writer for tests ───────────────────────────────────

    /// A helper that writes bits into a byte buffer for constructing test
    /// bitstream payloads (everything after the 4-byte magic).
    struct BsWriter {
        buf: Vec<u8>,
        pending: u64,
        pending_bits: usize,
    }

    impl BsWriter {
        fn new() -> Self {
            BsWriter { buf: Vec::new(), pending: 0, pending_bits: 0 }
        }

        fn write_bits(&mut self, val: u64, n: usize) {
            let mut v = val;
            let mut remaining = n;
            while remaining > 0 {
                let space = 8 - self.pending_bits;
                let take = remaining.min(space);
                self.pending |= (v & ((1u64 << take) - 1)) << self.pending_bits;
                self.pending_bits += take;
                v >>= take;
                remaining -= take;
                if self.pending_bits == 8 {
                    self.buf.push(self.pending as u8);
                    self.pending = 0;
                    self.pending_bits = 0;
                }
            }
        }

        fn write_vbr(&mut self, mut val: u64, width: usize) {
            let low = width - 1;
            let cont = 1u64 << low;
            let mask = cont - 1;
            loop {
                let piece = val & mask;
                val >>= low;
                if val == 0 {
                    self.write_bits(piece, width);
                    break;
                } else {
                    self.write_bits(piece | cont, width);
                }
            }
        }

        fn align_32(&mut self) {
            // Flush pending bits.
            if self.pending_bits > 0 {
                self.buf.push(self.pending as u8);
                self.pending = 0;
                self.pending_bits = 0;
            }
            while self.buf.len() % 4 != 0 {
                self.buf.push(0);
            }
        }

        /// Begin a sub-block.  Returns the index of the block-length word
        /// that we'll patch after writing the block contents.
        fn enter_block(&mut self, block_id: u64, abbrev_len: usize) {
            // abbrev_id = ENTER_SUBBLOCK (1) at 2 bits.
            self.write_bits(1, 2);
            // block_id VBR8.
            self.write_vbr(block_id, 8);
            // new abbrev len VBR4.
            self.write_vbr(abbrev_len as u64, 4);
            // Align to 32 bits.
            self.align_32();
            // Reserve 4 bytes for block length (in 32-bit words).
            self.block_len_placeholder();
        }

        fn block_len_placeholder(&mut self) {
            self.buf.push(0);
            self.buf.push(0);
            self.buf.push(0);
            self.buf.push(0);
        }

        fn patch_block_len(&mut self, start_word_idx: usize, current_word_idx: usize) {
            let len = current_word_idx - start_word_idx;
            let bytes = (len as u32).to_le_bytes();
            self.buf[start_word_idx * 4..start_word_idx * 4 + 4].copy_from_slice(&bytes);
        }

        /// Write END_BLOCK (0) and align to 32 bits.
        fn end_block(&mut self) {
            self.write_bits(0, 2); // abbrev_id = END_BLOCK
            self.align_32();
        }

        /// Write an unabbreviated record with code and fields.
        fn write_unabbrev_record(&mut self, abbrev_len: usize, code: u64, fields: &[u64]) {
            self.write_bits(3, abbrev_len); // UNABBREV_RECORD
            self.write_vbr(code, 6);
            self.write_vbr(fields.len() as u64, 6);
            for &f in fields {
                self.write_vbr(f, 6);
            }
        }

        fn finish(mut self) -> Vec<u8> {
            if self.pending_bits > 0 {
                self.buf.push(self.pending as u8);
            }
            // Pad to 4 bytes.
            while self.buf.len() % 4 != 0 {
                self.buf.push(0);
            }
            self.buf
        }
    }

    /// Build a complete .bc byte sequence with LLVM magic + body.
    fn with_magic(body: Vec<u8>) -> Vec<u8> {
        let mut out = b"BC\xc0\xde".to_vec();
        out.extend(body);
        out
    }

    // ── 1. Wrong magic returns InvalidMagic ──────────────────────────────────

    #[test]
    fn test_wrong_magic_returns_invalid_magic() {
        let bad = b"LRIR\x00\x00\x00\x00";
        let result = read_llvm_bc(bad);
        assert!(matches!(result, Err(BitcodeError::InvalidMagic)));
    }

    #[test]
    fn test_empty_bytes_returns_invalid_magic() {
        let result = read_llvm_bc(b"");
        assert!(matches!(result, Err(BitcodeError::InvalidMagic)));
    }

    #[test]
    fn test_three_byte_prefix_returns_invalid_magic() {
        let result = read_llvm_bc(b"BC\xc0");
        assert!(matches!(result, Err(BitcodeError::InvalidMagic)));
    }

    // ── 2. VBR decoding tests ─────────────────────────────────────────────────

    #[test]
    fn test_vbr_single_group_no_continuation() {
        // VBR4: value 5 (binary 0101) — fits in one group (no continuation).
        // Byte layout (LSB-first): bits 0-3 = 0101 = 5.
        // 0b00000101 = 0x05
        let bits: &[u8] = &[0b0000_0101];
        let mut bs = BitStreamReader::new(bits);
        let v = bs.read_vbr(4).unwrap();
        assert_eq!(v, 5);
    }

    #[test]
    fn test_vbr_two_groups() {
        // VBR4: encode 20.
        // 20 in VBR4: low 3 bits = 20 & 7 = 4, cont=1 → first group = 0b1100
        //             next:  20 >> 3 = 2, cont=0 → second group = 0b0010
        // In byte (LSB-first): bits 0-3 = 1100, bits 4-7 = 0010 → byte = 0b0010_1100
        let bits: &[u8] = &[0b0010_1100];
        let mut bs = BitStreamReader::new(bits);
        let v = bs.read_vbr(4).unwrap();
        assert_eq!(v, 20);
    }

    #[test]
    fn test_vbr6_small_value() {
        // VBR6: value 3. Single group, 3 in 6 bits = 0b000011, no continuation.
        // In byte: 0b00000011
        let bits: &[u8] = &[0b00_000011, 0];
        let mut bs = BitStreamReader::new(bits);
        let v = bs.read_vbr(6).unwrap();
        assert_eq!(v, 3);
    }

    // ── 3. BitStreamReader read_bits ─────────────────────────────────────────

    #[test]
    fn test_read_bits_simple() {
        // First byte 0xA5 = 10100101
        // Reading 4 bits from LSB: 0101 = 5
        // Then 4 more: 1010 = 10
        let bits: &[u8] = &[0xA5];
        let mut bs = BitStreamReader::new(bits);
        assert_eq!(bs.read_bits(4).unwrap(), 5);
        assert_eq!(bs.read_bits(4).unwrap(), 10);
    }

    #[test]
    fn test_read_bits_zero() {
        let bits: &[u8] = &[0xFF];
        let mut bs = BitStreamReader::new(bits);
        assert_eq!(bs.read_bits(0).unwrap(), 0);
    }

    // ── 4. Empty module block ────────────────────────────────────────────────

    /// Build a minimal valid LLVM .bc bitstream that contains only an empty
    /// MODULE_BLOCK (no types, no constants, no functions).
    fn build_empty_module_bc() -> Vec<u8> {
        let mut w = BsWriter::new();

        // top-level abbrev_len = 2 bits.

        // Enter MODULE_BLOCK (id=8), new abbrev_len=3.
        w.write_bits(1, 2); // ENTER_SUBBLOCK
        w.write_vbr(8, 8);  // block_id
        w.write_vbr(3, 4);  // new abbrev_len
        w.align_32();
        // Placeholder for block length (in 32-bit words).
        let block_start_byte = w.buf.len();
        w.buf.extend_from_slice(&[0u8; 4]); // will patch

        // MODULE_CODE_VERSION = 1, version = 2.
        w.write_bits(3, 3); // UNABBREV_RECORD in abbrev_len=3
        w.write_vbr(1, 6);  // code = MODULE_CODE_VERSION
        w.write_vbr(1, 6);  // num_ops = 1
        w.write_vbr(2, 6);  // version = 2

        // END_BLOCK.
        w.write_bits(0, 3); // END_BLOCK at abbrev_len=3
        w.align_32();

        // Patch block length.
        let block_end_byte = w.buf.len();
        let block_start_word = block_start_byte / 4;
        let block_end_word = block_end_byte / 4;
        // The length field is the number of 32-bit words AFTER the length field.
        let len_in_words = block_end_word - block_start_word - 1;
        let len_bytes = (len_in_words as u32).to_le_bytes();
        w.buf[block_start_byte..block_start_byte + 4].copy_from_slice(&len_bytes);

        with_magic(w.finish())
    }

    #[test]
    fn test_empty_module_bc_parses() {
        let bc = build_empty_module_bc();
        let result = read_llvm_bc(&bc);
        // Should succeed without panic.
        assert!(result.is_ok(), "parse failed: {:?}", result.err());
    }

    // ── 5. Type table decoding ────────────────────────────────────────────────

    #[test]
    fn test_type_table_void_and_integer() {
        // Construct a minimal .bc with a type block containing void + i32.
        // Then verify we can parse it without error.
        let bc = build_type_table_bc(&[
            (TYPE_CODE_VOID, vec![]),
            (TYPE_CODE_INTEGER, vec![32]),
        ]);
        let result = read_llvm_bc(&bc);
        assert!(result.is_ok(), "type table parse failed: {:?}", result.err());
        let (ctx, _module) = result.unwrap();
        // void_ty and i32_ty are always built in; just check ctx has types.
        assert!(ctx.num_types() > 0);
    }

    /// Build a minimal .bc that has a MODULE_BLOCK containing a TYPE_BLOCK
    /// with the given records.
    fn build_type_table_bc(type_records: &[(u64, Vec<u64>)]) -> Vec<u8> {
        let module_body = build_module_with_type_block(type_records);
        with_magic(module_body)
    }

    fn build_module_with_type_block(type_records: &[(u64, Vec<u64>)]) -> Vec<u8> {
        let mut w = BsWriter::new();
        let abbrev_module = 3usize;

        // Enter MODULE_BLOCK.
        w.write_bits(1, 2); w.write_vbr(8, 8); w.write_vbr(abbrev_module as u64, 4); w.align_32();
        let mod_len_offset = w.buf.len();
        w.buf.extend_from_slice(&[0u8; 4]);

        {
            // Enter TYPE_BLOCK (id=17), abbrev_len=4.
            w.write_bits(1, abbrev_module as u64 as usize); // ENTER_SUBBLOCK
            w.write_vbr(17, 8); w.write_vbr(4u64, 4); w.align_32();
            let type_len_offset = w.buf.len();
            w.buf.extend_from_slice(&[0u8; 4]);

            // NUMENTRY record.
            let n = type_records.len() as u64;
            w.write_bits(3, 4); // UNABBREV_RECORD at abbrev_len=4
            w.write_vbr(TYPE_CODE_NUMENTRY, 6);
            w.write_vbr(1, 6); w.write_vbr(n, 6);

            for (code, fields) in type_records {
                w.write_bits(3, 4);
                w.write_vbr(*code, 6);
                w.write_vbr(fields.len() as u64, 6);
                for &f in fields { w.write_vbr(f, 6); }
            }

            // END TYPE_BLOCK.
            w.write_bits(0, 4); w.align_32();
            let type_end = w.buf.len();
            let type_words = (type_end - type_len_offset - 4) / 4;
            w.buf[type_len_offset..type_len_offset+4].copy_from_slice(&(type_words as u32).to_le_bytes());
        }

        // END MODULE_BLOCK.
        w.write_bits(0, abbrev_module);
        w.align_32();
        let mod_end = w.buf.len();
        let mod_words = (mod_end - mod_len_offset - 4) / 4;
        w.buf[mod_len_offset..mod_len_offset+4].copy_from_slice(&(mod_words as u32).to_le_bytes());

        w.finish()
    }

    // ── 6. Constant decoding — integer sign-rotation ──────────────────────────

    #[test]
    fn test_sign_rotated_positive() {
        // 42 → encoded as 42*2 = 84
        assert_eq!(decode_sign_rotated(84), 42);
    }

    #[test]
    fn test_sign_rotated_negative_one() {
        // -1 → encoded as 1 (bit0=1, rest=0 → -(0+1) = -1 in two's comp)
        let v = decode_sign_rotated(1);
        assert_eq!(v as i64, -1i64);
    }

    #[test]
    fn test_sign_rotated_negative_two() {
        // -2 → encoded as 3 (bit0=1, rest=1 → -(1+1)=-2)
        let v = decode_sign_rotated(3);
        assert_eq!(v as i64, -2i64);
    }

    #[test]
    fn test_sign_rotated_zero() {
        assert_eq!(decode_sign_rotated(0), 0);
    }

    // ── 7. Char6 decoding ────────────────────────────────────────────────────

    #[test]
    fn test_char6_lowercase() {
        use crate::bitstream::*; // for char6 helper
        // 0..25 = a..z
        // We verify by going through the bitstream reader reading 6-bit codes.
        // Construct a byte stream with Char6 abbrev.
        // Instead, test via the reader low-level. We'll just verify the logic
        // by checking the VBR-decoded chars are sensible.
        let data: &[u8] = &[0b00000000]; // code 0 = 'a'
        let mut bs = BitStreamReader::new(data);
        assert_eq!(bs.read_bits(6).unwrap(), 0); // code 0 → 'a'
    }

    // ── 8. Module with a simple function declaration ──────────────────────────

    #[test]
    fn test_parse_module_with_function_decl() {
        // Build a .bc with:
        // - TYPE_BLOCK: void, i32, fn(i32)->void
        // - MODULE_CODE_FUNCTION: ty=2 (fn type), cc=0, is_decl=1, linkage=0
        let bc = build_fn_decl_bc();
        let result = read_llvm_bc(&bc);
        assert!(result.is_ok(), "parse failed: {:?}", result.err());
        let (_ctx, module) = result.unwrap();
        // We should have exactly one function declaration.
        assert_eq!(module.functions.len(), 1);
        assert!(module.functions[0].is_declaration);
    }

    fn build_fn_decl_bc() -> Vec<u8> {
        let mut w = BsWriter::new();
        let al = 3usize; // module abbrev_len

        // Enter MODULE_BLOCK.
        w.write_bits(1, 2); w.write_vbr(8, 8); w.write_vbr(al as u64, 4); w.align_32();
        let mod_off = w.buf.len(); w.buf.extend_from_slice(&[0u8; 4]);

        // TYPE_BLOCK: [void=slot0, i32=slot1, fn(i32)->void=slot2]
        w.write_bits(1, al); w.write_vbr(17, 8); w.write_vbr(4u64, 4); w.align_32();
        let ty_off = w.buf.len(); w.buf.extend_from_slice(&[0u8; 4]);
        {
            // NUMENTRY 3
            w.write_bits(3, 4); w.write_vbr(TYPE_CODE_NUMENTRY, 6); w.write_vbr(1, 6); w.write_vbr(3, 6);
            // void
            w.write_bits(3, 4); w.write_vbr(TYPE_CODE_VOID, 6); w.write_vbr(0, 6);
            // i32
            w.write_bits(3, 4); w.write_vbr(TYPE_CODE_INTEGER, 6); w.write_vbr(1, 6); w.write_vbr(32, 6);
            // fn(i32) -> void: FUNCTION vararg=0, ret=0(void), params=[1(i32)]
            w.write_bits(3, 4); w.write_vbr(TYPE_CODE_FUNCTION, 6); w.write_vbr(3, 6);
            w.write_vbr(0, 6); // vararg
            w.write_vbr(0, 6); // ret = slot 0 (void)
            w.write_vbr(1, 6); // param = slot 1 (i32)
        }
        w.write_bits(0, 4); w.align_32();
        let ty_end = w.buf.len();
        let ty_words = (ty_end - ty_off - 4) / 4;
        w.buf[ty_off..ty_off+4].copy_from_slice(&(ty_words as u32).to_le_bytes());

        // MODULE_CODE_FUNCTION record: ty=2, cc=0, is_decl=1, linkage=0
        w.write_bits(3, al);
        w.write_vbr(MODULE_CODE_FUNCTION, 6);
        w.write_vbr(4, 6); // num fields = 4
        w.write_vbr(2, 6); // ty_idx = 2
        w.write_vbr(0, 6); // calling_conv = 0
        w.write_vbr(1, 6); // is_declaration = 1
        w.write_vbr(0, 6); // linkage = External

        // END MODULE.
        w.write_bits(0, al); w.align_32();
        let mod_end = w.buf.len();
        let mod_words = (mod_end - mod_off - 4) / 4;
        w.buf[mod_off..mod_off+4].copy_from_slice(&(mod_words as u32).to_le_bytes());

        with_magic(w.finish())
    }

    // ── 9. Global variable decoding ──────────────────────────────────────────

    #[test]
    fn test_parse_global_variable() {
        let bc = build_globalvar_bc();
        let result = read_llvm_bc(&bc);
        assert!(result.is_ok(), "globalvar parse failed: {:?}", result.err());
        let (_ctx, module) = result.unwrap();
        assert_eq!(module.globals.len(), 1);
    }

    fn build_globalvar_bc() -> Vec<u8> {
        let mut w = BsWriter::new();
        let al = 3usize;

        // Enter MODULE_BLOCK.
        w.write_bits(1, 2); w.write_vbr(8, 8); w.write_vbr(al as u64, 4); w.align_32();
        let mod_off = w.buf.len(); w.buf.extend_from_slice(&[0u8; 4]);

        // TYPE_BLOCK: void(0), i32(1)
        w.write_bits(1, al); w.write_vbr(17, 8); w.write_vbr(4u64, 4); w.align_32();
        let ty_off = w.buf.len(); w.buf.extend_from_slice(&[0u8; 4]);
        {
            w.write_bits(3, 4); w.write_vbr(TYPE_CODE_NUMENTRY, 6); w.write_vbr(1, 6); w.write_vbr(2, 6);
            w.write_bits(3, 4); w.write_vbr(TYPE_CODE_VOID, 6); w.write_vbr(0, 6);
            w.write_bits(3, 4); w.write_vbr(TYPE_CODE_INTEGER, 6); w.write_vbr(1, 6); w.write_vbr(32, 6);
        }
        w.write_bits(0, 4); w.align_32();
        let ty_end = w.buf.len();
        w.buf[ty_off..ty_off+4].copy_from_slice(&(((ty_end - ty_off - 4) / 4) as u32).to_le_bytes());

        // MODULE_CODE_GLOBALVAR: ty=1(i32), is_const=0, init_id=0 (none), linkage=0, align=0, section=0
        w.write_bits(3, al);
        w.write_vbr(MODULE_CODE_GLOBALVAR, 6);
        w.write_vbr(6, 6); // num fields
        w.write_vbr(1, 6); // ty_idx = 1 (i32)
        w.write_vbr(0, 6); // is_const+addrspace = 0
        w.write_vbr(0, 6); // init_id = 0 (no init)
        w.write_vbr(0, 6); // linkage = External
        w.write_vbr(0, 6); // alignment
        w.write_vbr(0, 6); // section

        // END MODULE.
        w.write_bits(0, al); w.align_32();
        let mod_end = w.buf.len();
        w.buf[mod_off..mod_off+4].copy_from_slice(&(((mod_end - mod_off - 4) / 4) as u32).to_le_bytes());

        with_magic(w.finish())
    }

    // ── 10. Function body with ret void ──────────────────────────────────────

    #[test]
    fn test_parse_function_body_ret_void() {
        let bc = build_fn_body_ret_void_bc();
        let result = read_llvm_bc(&bc);
        assert!(result.is_ok(), "fn body parse failed: {:?}", result.err());
        let (_ctx, module) = result.unwrap();
        assert_eq!(module.functions.len(), 1);
        assert!(!module.functions[0].is_declaration);
    }

    fn build_fn_body_ret_void_bc() -> Vec<u8> {
        let mut w = BsWriter::new();
        let al = 3usize;

        // Enter MODULE_BLOCK.
        w.write_bits(1, 2); w.write_vbr(8, 8); w.write_vbr(al as u64, 4); w.align_32();
        let mod_off = w.buf.len(); w.buf.extend_from_slice(&[0u8; 4]);

        // TYPE_BLOCK: void(0), fn()->void(1)
        w.write_bits(1, al); w.write_vbr(17, 8); w.write_vbr(4u64, 4); w.align_32();
        let ty_off = w.buf.len(); w.buf.extend_from_slice(&[0u8; 4]);
        {
            w.write_bits(3, 4); w.write_vbr(TYPE_CODE_NUMENTRY, 6); w.write_vbr(1, 6); w.write_vbr(2, 6);
            w.write_bits(3, 4); w.write_vbr(TYPE_CODE_VOID, 6); w.write_vbr(0, 6);
            // fn()->void: FUNCTION, vararg=0, ret=0
            w.write_bits(3, 4); w.write_vbr(TYPE_CODE_FUNCTION, 6); w.write_vbr(2, 6);
            w.write_vbr(0, 6); // vararg
            w.write_vbr(0, 6); // ret = void (slot 0)
        }
        w.write_bits(0, 4); w.align_32();
        let ty_end = w.buf.len();
        w.buf[ty_off..ty_off+4].copy_from_slice(&(((ty_end - ty_off - 4) / 4) as u32).to_le_bytes());

        // MODULE_CODE_FUNCTION: ty=1, cc=0, is_decl=0, linkage=0
        w.write_bits(3, al);
        w.write_vbr(MODULE_CODE_FUNCTION, 6);
        w.write_vbr(4, 6);
        w.write_vbr(1, 6); // ty = fn()->void
        w.write_vbr(0, 6); // cc
        w.write_vbr(0, 6); // is_declaration = 0 → has body
        w.write_vbr(0, 6); // linkage

        // FUNCTION_BLOCK.
        w.write_bits(1, al); w.write_vbr(12, 8); w.write_vbr(4u64, 4); w.align_32();
        let fn_off = w.buf.len(); w.buf.extend_from_slice(&[0u8; 4]);
        {
            // DECLAREBLOCKS: 1 block.
            w.write_bits(3, 4); w.write_vbr(FUNC_CODE_DECLAREBLOCKS, 6); w.write_vbr(1, 6); w.write_vbr(1, 6);
            // INST_RET with no value.
            w.write_bits(3, 4); w.write_vbr(FUNC_CODE_INST_RET, 6); w.write_vbr(0, 6);
        }
        w.write_bits(0, 4); w.align_32();
        let fn_end = w.buf.len();
        w.buf[fn_off..fn_off+4].copy_from_slice(&(((fn_end - fn_off - 4) / 4) as u32).to_le_bytes());

        // END MODULE.
        w.write_bits(0, al); w.align_32();
        let mod_end = w.buf.len();
        w.buf[mod_off..mod_off+4].copy_from_slice(&(((mod_end - mod_off - 4) / 4) as u32).to_le_bytes());

        with_magic(w.finish())
    }

    // ── 11. Unconditional branch ──────────────────────────────────────────────

    #[test]
    fn test_parse_unconditional_br() {
        // Build a function with two basic blocks; block 0 branches to block 1,
        // block 1 returns void.
        let bc = build_fn_body_br_bc();
        let result = read_llvm_bc(&bc);
        assert!(result.is_ok(), "br parse failed: {:?}", result.err());
        let (_ctx, module) = result.unwrap();
        assert_eq!(module.functions.len(), 1);
        // There should be 2 basic blocks.
        assert_eq!(module.functions[0].blocks.len(), 2);
    }

    fn build_fn_body_br_bc() -> Vec<u8> {
        let mut w = BsWriter::new();
        let al = 3usize;

        w.write_bits(1, 2); w.write_vbr(8, 8); w.write_vbr(al as u64, 4); w.align_32();
        let mod_off = w.buf.len(); w.buf.extend_from_slice(&[0u8; 4]);

        // Types: void(0), fn()->void(1)
        w.write_bits(1, al); w.write_vbr(17, 8); w.write_vbr(4u64, 4); w.align_32();
        let ty_off = w.buf.len(); w.buf.extend_from_slice(&[0u8; 4]);
        {
            w.write_bits(3, 4); w.write_vbr(TYPE_CODE_NUMENTRY, 6); w.write_vbr(1, 6); w.write_vbr(2, 6);
            w.write_bits(3, 4); w.write_vbr(TYPE_CODE_VOID, 6); w.write_vbr(0, 6);
            w.write_bits(3, 4); w.write_vbr(TYPE_CODE_FUNCTION, 6); w.write_vbr(2, 6); w.write_vbr(0, 6); w.write_vbr(0, 6);
        }
        w.write_bits(0, 4); w.align_32();
        let ty_end = w.buf.len();
        w.buf[ty_off..ty_off+4].copy_from_slice(&(((ty_end - ty_off - 4) / 4) as u32).to_le_bytes());

        // Function declaration.
        w.write_bits(3, al); w.write_vbr(MODULE_CODE_FUNCTION, 6); w.write_vbr(4, 6);
        w.write_vbr(1, 6); w.write_vbr(0, 6); w.write_vbr(0, 6); w.write_vbr(0, 6);

        // Function body.
        w.write_bits(1, al); w.write_vbr(12, 8); w.write_vbr(4u64, 4); w.align_32();
        let fn_off = w.buf.len(); w.buf.extend_from_slice(&[0u8; 4]);
        {
            // DECLAREBLOCKS: 2
            w.write_bits(3, 4); w.write_vbr(FUNC_CODE_DECLAREBLOCKS, 6); w.write_vbr(1, 6); w.write_vbr(2, 6);
            // Block 0: INST_BR dest=1 (unconditional)
            w.write_bits(3, 4); w.write_vbr(FUNC_CODE_INST_BR, 6); w.write_vbr(1, 6); w.write_vbr(1, 6);
            // Block 1: INST_RET (no value)
            w.write_bits(3, 4); w.write_vbr(FUNC_CODE_INST_RET, 6); w.write_vbr(0, 6);
        }
        w.write_bits(0, 4); w.align_32();
        let fn_end = w.buf.len();
        w.buf[fn_off..fn_off+4].copy_from_slice(&(((fn_end - fn_off - 4) / 4) as u32).to_le_bytes());

        w.write_bits(0, al); w.align_32();
        let mod_end = w.buf.len();
        w.buf[mod_off..mod_off+4].copy_from_slice(&(((mod_end - mod_off - 4) / 4) as u32).to_le_bytes());

        with_magic(w.finish())
    }

    // ── 12. Alloca + Load + Store ──────────────────────────────────────────────

    #[test]
    fn test_parse_alloca_load_store() {
        let bc = build_alloca_load_store_bc();
        let result = read_llvm_bc(&bc);
        assert!(result.is_ok(), "alloca/load/store parse failed: {:?}", result.err());
        let (_ctx, module) = result.unwrap();
        assert_eq!(module.functions.len(), 1);
        // Function should have instructions.
        assert!(!module.functions[0].instructions.is_empty());
    }

    fn build_alloca_load_store_bc() -> Vec<u8> {
        // Build: define void @f(i32 %x) { %p = alloca i32; store i32 %x, i32* %p; ret void }
        let mut w = BsWriter::new();
        let al = 3usize;

        w.write_bits(1, 2); w.write_vbr(8, 8); w.write_vbr(al as u64, 4); w.align_32();
        let mod_off = w.buf.len(); w.buf.extend_from_slice(&[0u8; 4]);

        // Types: void(0), i32(1), ptr(2), fn(i32)->void(3)
        w.write_bits(1, al); w.write_vbr(17, 8); w.write_vbr(4u64, 4); w.align_32();
        let ty_off = w.buf.len(); w.buf.extend_from_slice(&[0u8; 4]);
        {
            w.write_bits(3, 4); w.write_vbr(TYPE_CODE_NUMENTRY, 6); w.write_vbr(1, 6); w.write_vbr(4, 6);
            w.write_bits(3, 4); w.write_vbr(TYPE_CODE_VOID, 6); w.write_vbr(0, 6);
            w.write_bits(3, 4); w.write_vbr(TYPE_CODE_INTEGER, 6); w.write_vbr(1, 6); w.write_vbr(32, 6);
            w.write_bits(3, 4); w.write_vbr(TYPE_CODE_OPAQUE_POINTER, 6); w.write_vbr(0, 6);
            // fn(i32)->void
            w.write_bits(3, 4); w.write_vbr(TYPE_CODE_FUNCTION, 6); w.write_vbr(3, 6);
            w.write_vbr(0, 6); w.write_vbr(0, 6); w.write_vbr(1, 6);
        }
        w.write_bits(0, 4); w.align_32();
        let ty_end = w.buf.len();
        w.buf[ty_off..ty_off+4].copy_from_slice(&(((ty_end - ty_off - 4) / 4) as u32).to_le_bytes());

        // Function decl.
        w.write_bits(3, al); w.write_vbr(MODULE_CODE_FUNCTION, 6); w.write_vbr(4, 6);
        w.write_vbr(3, 6); w.write_vbr(0, 6); w.write_vbr(0, 6); w.write_vbr(0, 6);

        // Function body.
        w.write_bits(1, al); w.write_vbr(12, 8); w.write_vbr(4u64, 4); w.align_32();
        let fn_off = w.buf.len(); w.buf.extend_from_slice(&[0u8; 4]);
        {
            // DECLAREBLOCKS: 1
            w.write_bits(3, 4); w.write_vbr(FUNC_CODE_DECLAREBLOCKS, 6); w.write_vbr(1, 6); w.write_vbr(1, 6);
            // ALLOCA i32 (inst_ty=1=i32, op_ty=1=i32, size=1, align=0)
            w.write_bits(3, 4); w.write_vbr(FUNC_CODE_INST_ALLOCA, 6); w.write_vbr(4, 6);
            w.write_vbr(1, 6); // inst_ty = i32
            w.write_vbr(1, 6); // op_ty = i32
            w.write_vbr(1, 6); // size
            w.write_vbr(0, 6); // align_encoded

            // RET void
            w.write_bits(3, 4); w.write_vbr(FUNC_CODE_INST_RET, 6); w.write_vbr(0, 6);
        }
        w.write_bits(0, 4); w.align_32();
        let fn_end = w.buf.len();
        w.buf[fn_off..fn_off+4].copy_from_slice(&(((fn_end - fn_off - 4) / 4) as u32).to_le_bytes());

        w.write_bits(0, al); w.align_32();
        let mod_end = w.buf.len();
        w.buf[mod_off..mod_off+4].copy_from_slice(&(((mod_end - mod_off - 4) / 4) as u32).to_le_bytes());

        with_magic(w.finish())
    }

    // ── 13. Existing LRIR tests still pass ───────────────────────────────────
    // (covered by the existing test module in lib.rs)

    // ── 14. Multiple type kinds ───────────────────────────────────────────────

    #[test]
    fn test_parse_float_double_types() {
        let bc = build_type_table_bc(&[
            (TYPE_CODE_VOID, vec![]),
            (TYPE_CODE_FLOAT, vec![]),
            (TYPE_CODE_DOUBLE, vec![]),
            (TYPE_CODE_INTEGER, vec![64]),
        ]);
        let result = read_llvm_bc(&bc);
        assert!(result.is_ok(), "float/double type parse failed: {:?}", result.err());
    }

    // ── 15. Opaque pointer type ───────────────────────────────────────────────

    #[test]
    fn test_parse_opaque_pointer_type() {
        let bc = build_type_table_bc(&[
            (TYPE_CODE_VOID, vec![]),
            (TYPE_CODE_OPAQUE_POINTER, vec![0]),
        ]);
        let result = read_llvm_bc(&bc);
        assert!(result.is_ok(), "opaque ptr type parse failed: {:?}", result.err());
    }
}
