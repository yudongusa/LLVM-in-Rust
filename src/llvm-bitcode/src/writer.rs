//! Bitcode writer: serializes an IR `Module` to the LRIR binary format.
//!
//! # Format
//!
//! The LRIR ("LLVM-in-Rust IR") binary format is a custom format designed for
//! compact, faithful round-trip serialization of a `(Context, Module)` pair.
//!
//! Layout:
//! ```text
//! [4B]  magic  = 0x4C 0x52 0x49 0x52  ("LRIR")
//! [4B]  version (u32 LE) = 1
//! [4B]  type_count (u32 LE)
//! [...]  type_count × TypeRecord
//! [4B]  const_count (u32 LE)
//! [...]  const_count × ConstRecord
//! [str]  module_name (u32 len + bytes)
//! [4B]  func_count (u32 LE)
//! [...]  func_count × FunctionRecord
//! ```
//!
//! Each string is encoded as `u32 length` + UTF-8 bytes (no null terminator).
//! Optional strings use a 0 length to mean "absent".

use llvm_ir::{
    BasicBlock, ConstantData, Context, FloatKind, Function, InstrKind, Instruction, Module,
    TypeData, ValueRef,
};

/// Serialize `(ctx, module)` into the LRIR binary format.
///
/// Returns the encoded bytes.  The encoding is always valid; no `Result` is
/// needed because we never encounter unrepresentable values in the IR model.
pub fn write_bitcode(ctx: &Context, module: &Module) -> Vec<u8> {
    let mut w = Writer::default();

    // ── header ────────────────────────────────────────────────────────────
    w.raw(b"LRIR");
    w.u32(1); // version

    // ── type table ────────────────────────────────────────────────────────
    let type_count = ctx.num_types() as u32;
    w.u32(type_count);
    for (_, td) in ctx.types() {
        encode_type(&mut w, td);
    }

    // ── constant table ─────────────────────────────────────────────────────
    let const_count = ctx.constants.len() as u32;
    w.u32(const_count);
    for cd in &ctx.constants {
        encode_const(&mut w, cd);
    }

    // ── module header ──────────────────────────────────────────────────────
    w.string(&module.name);

    // ── functions ──────────────────────────────────────────────────────────
    w.u32(module.functions.len() as u32);
    for func in &module.functions {
        encode_function(&mut w, func);
    }

    w.buf
}

// ── type encoding ──────────────────────────────────────────────────────────

/// Tag bytes for type records.
mod type_tag {
    pub const VOID: u8 = 0;
    pub const INTEGER: u8 = 1;
    pub const FLOAT: u8 = 2;
    pub const POINTER: u8 = 3;
    pub const ARRAY: u8 = 4;
    pub const VECTOR: u8 = 5;
    pub const STRUCT: u8 = 6;
    pub const FUNCTION: u8 = 7;
    pub const LABEL: u8 = 8;
    pub const METADATA: u8 = 9;
}

mod float_tag {
    pub const HALF: u8 = 0;
    pub const BFLOAT: u8 = 1;
    pub const SINGLE: u8 = 2;
    pub const DOUBLE: u8 = 3;
    pub const FP128: u8 = 4;
    pub const X86FP80: u8 = 5;
}

fn encode_type(w: &mut Writer, td: &TypeData) {
    match td {
        TypeData::Void => {
            w.u8(type_tag::VOID);
        }
        TypeData::Integer(bits) => {
            w.u8(type_tag::INTEGER);
            w.u32(*bits);
        }
        TypeData::Float(kind) => {
            w.u8(type_tag::FLOAT);
            let tag = match kind {
                FloatKind::Half => float_tag::HALF,
                FloatKind::BFloat => float_tag::BFLOAT,
                FloatKind::Single => float_tag::SINGLE,
                FloatKind::Double => float_tag::DOUBLE,
                FloatKind::Fp128 => float_tag::FP128,
                FloatKind::X86Fp80 => float_tag::X86FP80,
            };
            w.u8(tag);
        }
        TypeData::Pointer => {
            w.u8(type_tag::POINTER);
        }
        TypeData::Array { element, len } => {
            w.u8(type_tag::ARRAY);
            w.u32(element.0);
            w.u64(*len);
        }
        TypeData::Vector {
            element,
            len,
            scalable,
        } => {
            w.u8(type_tag::VECTOR);
            w.u32(element.0);
            w.u32(*len);
            w.u8(if *scalable { 1 } else { 0 });
        }
        TypeData::Struct(st) => {
            w.u8(type_tag::STRUCT);
            // Optional name.
            match &st.name {
                Some(n) => w.string(n),
                None => w.u32(0),
            }
            w.u8(if st.packed { 1 } else { 0 });
            w.u32(st.fields.len() as u32);
            for &f in &st.fields {
                w.u32(f.0);
            }
        }
        TypeData::Function(ft) => {
            w.u8(type_tag::FUNCTION);
            w.u32(ft.ret.0);
            w.u8(if ft.variadic { 1 } else { 0 });
            w.u32(ft.params.len() as u32);
            for &p in &ft.params {
                w.u32(p.0);
            }
        }
        TypeData::Label => {
            w.u8(type_tag::LABEL);
        }
        TypeData::Metadata => {
            w.u8(type_tag::METADATA);
        }
    }
}

// ── constant encoding ─────────────────────────────────────────────────────

mod const_tag {
    pub const INT: u8 = 0;
    pub const INT_WIDE: u8 = 1;
    pub const FLOAT: u8 = 2;
    pub const NULL: u8 = 3;
    pub const UNDEF: u8 = 4;
    pub const POISON: u8 = 5;
    pub const ZERO_INIT: u8 = 6;
    pub const ARRAY: u8 = 7;
    pub const STRUCT: u8 = 8;
    pub const VECTOR: u8 = 9;
    pub const GLOBAL_REF: u8 = 10;
}

fn encode_const(w: &mut Writer, cd: &ConstantData) {
    match cd {
        ConstantData::Int { ty, val } => {
            w.u8(const_tag::INT);
            w.u32(ty.0);
            w.u64(*val);
        }
        ConstantData::IntWide { ty, words } => {
            w.u8(const_tag::INT_WIDE);
            w.u32(ty.0);
            w.u32(words.len() as u32);
            for &word in words {
                w.u64(word);
            }
        }
        ConstantData::Float { ty, bits } => {
            w.u8(const_tag::FLOAT);
            w.u32(ty.0);
            w.u64(*bits);
        }
        ConstantData::Null(ty) => {
            w.u8(const_tag::NULL);
            w.u32(ty.0);
        }
        ConstantData::Undef(ty) => {
            w.u8(const_tag::UNDEF);
            w.u32(ty.0);
        }
        ConstantData::Poison(ty) => {
            w.u8(const_tag::POISON);
            w.u32(ty.0);
        }
        ConstantData::ZeroInitializer(ty) => {
            w.u8(const_tag::ZERO_INIT);
            w.u32(ty.0);
        }
        ConstantData::Array { ty, elements } => {
            w.u8(const_tag::ARRAY);
            w.u32(ty.0);
            w.u32(elements.len() as u32);
            for &e in elements {
                w.u32(e.0);
            }
        }
        ConstantData::Struct { ty, fields } => {
            w.u8(const_tag::STRUCT);
            w.u32(ty.0);
            w.u32(fields.len() as u32);
            for &f in fields {
                w.u32(f.0);
            }
        }
        ConstantData::Vector { ty, elements } => {
            w.u8(const_tag::VECTOR);
            w.u32(ty.0);
            w.u32(elements.len() as u32);
            for &e in elements {
                w.u32(e.0);
            }
        }
        ConstantData::GlobalRef { ty, id, name } => {
            w.u8(const_tag::GLOBAL_REF);
            w.u32(ty.0);
            w.u32(id.0);
            w.string(name);
        }
    }
}

// ── function encoding ─────────────────────────────────────────────────────

mod linkage_tag {
    pub const PRIVATE: u8 = 0;
    pub const INTERNAL: u8 = 1;
    pub const EXTERNAL: u8 = 2;
    pub const WEAK: u8 = 3;
    pub const WEAK_ODR: u8 = 4;
    pub const LINK_ONCE: u8 = 5;
    pub const LINK_ONCE_ODR: u8 = 6;
    pub const COMMON: u8 = 7;
    pub const AVAILABLE_EXTERNALLY: u8 = 8;
}

fn encode_function(w: &mut Writer, func: &Function) {
    w.string(&func.name);
    w.u32(func.ty.0);
    // Linkage.
    use llvm_ir::Linkage;
    let ltag = match func.linkage {
        Linkage::Private => linkage_tag::PRIVATE,
        Linkage::Internal => linkage_tag::INTERNAL,
        Linkage::External => linkage_tag::EXTERNAL,
        Linkage::Weak => linkage_tag::WEAK,
        Linkage::WeakOdr => linkage_tag::WEAK_ODR,
        Linkage::LinkOnce => linkage_tag::LINK_ONCE,
        Linkage::LinkOnceOdr => linkage_tag::LINK_ONCE_ODR,
        Linkage::Common => linkage_tag::COMMON,
        Linkage::AvailableExternally => linkage_tag::AVAILABLE_EXTERNALLY,
    };
    w.u8(ltag);
    w.u8(if func.is_declaration { 1 } else { 0 });

    // Arguments.
    w.u32(func.args.len() as u32);
    for arg in &func.args {
        w.string(&arg.name);
        w.u32(arg.ty.0);
        w.u32(arg.index);
    }

    // Basic blocks.
    w.u32(func.blocks.len() as u32);
    for bb in &func.blocks {
        encode_block(w, bb, func);
    }

    // Flat instruction pool (used for round-trip; type info is embedded here).
    w.u32(func.instructions.len() as u32);
    for instr in &func.instructions {
        encode_instr(w, instr);
    }
}

fn encode_block(w: &mut Writer, bb: &BasicBlock, func: &Function) {
    w.string(&bb.name);
    w.u32(bb.body.len() as u32);
    for &iid in &bb.body {
        w.u32(iid.0);
    }
    // Terminator: 0xFFFFFFFF means None.
    match bb.terminator {
        Some(tid) => w.u32(tid.0),
        None => w.u32(0xFFFF_FFFF),
    }
    let _ = func;
}

// ── instruction encoding ──────────────────────────────────────────────────
//
// We encode InstrKind as a tag + operands.
// For the full round-trip, we need the instruction name, type, and kind.

mod instr_tag {
    pub const ADD: u32 = 0;
    pub const SUB: u32 = 1;
    pub const MUL: u32 = 2;
    pub const UDIV: u32 = 3;
    pub const SDIV: u32 = 4;
    pub const UREM: u32 = 5;
    pub const SREM: u32 = 6;
    pub const AND: u32 = 10;
    pub const OR: u32 = 11;
    pub const XOR: u32 = 12;
    pub const SHL: u32 = 13;
    pub const LSHR: u32 = 14;
    pub const ASHR: u32 = 15;
    pub const FADD: u32 = 20;
    pub const FSUB: u32 = 21;
    pub const FMUL: u32 = 22;
    pub const FDIV: u32 = 23;
    pub const FREM: u32 = 24;
    pub const FNEG: u32 = 25;
    pub const ICMP: u32 = 30;
    pub const FCMP: u32 = 31;
    pub const ALLOCA: u32 = 40;
    pub const LOAD: u32 = 41;
    pub const STORE: u32 = 42;
    pub const GEP: u32 = 43;
    pub const TRUNC: u32 = 50;
    pub const ZEXT: u32 = 51;
    pub const SEXT: u32 = 52;
    pub const FPTRUNC: u32 = 53;
    pub const FPEXT: u32 = 54;
    pub const FPTOUI: u32 = 55;
    pub const FPTOSI: u32 = 56;
    pub const UITOFP: u32 = 57;
    pub const SITOFP: u32 = 58;
    pub const PTRTOINT: u32 = 59;
    pub const INTTOPTR: u32 = 60;
    pub const BITCAST: u32 = 61;
    pub const ADDRSPACECAST: u32 = 62;
    pub const SELECT: u32 = 70;
    pub const PHI: u32 = 71;
    pub const EXTRACTVALUE: u32 = 72;
    pub const INSERTVALUE: u32 = 73;
    pub const EXTRACTELEM: u32 = 74;
    pub const INSERTELEM: u32 = 75;
    pub const SHUFFLEVEC: u32 = 76;
    pub const CALL: u32 = 80;
    pub const RET: u32 = 90;
    pub const BR: u32 = 91;
    pub const CONDBR: u32 = 92;
    pub const SWITCH: u32 = 93;
    pub const UNREACHABLE: u32 = 94;
}

fn encode_vref(w: &mut Writer, vr: &ValueRef) {
    match vr {
        ValueRef::Instruction(id) => {
            w.u8(0);
            w.u32(id.0);
        }
        ValueRef::Argument(id) => {
            w.u8(1);
            w.u32(id.0);
        }
        ValueRef::Constant(id) => {
            w.u8(2);
            w.u32(id.0);
        }
        ValueRef::Global(id) => {
            w.u8(3);
            w.u32(id.0);
        }
    }
}

fn encode_opt_vref(w: &mut Writer, ovr: &Option<ValueRef>) {
    match ovr {
        Some(vr) => {
            w.u8(1);
            encode_vref(w, vr);
        }
        None => {
            w.u8(0);
        }
    }
}

fn encode_instr(w: &mut Writer, instr: &Instruction) {
    // Name: empty string → unnamed.
    match &instr.name {
        Some(n) => w.string(n),
        None => w.u32(0),
    }
    // Result type.
    w.u32(instr.ty.0);

    // Kind tag + operands.
    use InstrKind::*;
    match &instr.kind {
        Add {
            flags, lhs, rhs, ..
        } => {
            w.u32(instr_tag::ADD);
            w.u8(if flags.nuw { 1 } else { 0 });
            w.u8(if flags.nsw { 1 } else { 0 });
            encode_vref(w, lhs);
            encode_vref(w, rhs);
        }
        Sub {
            flags, lhs, rhs, ..
        } => {
            w.u32(instr_tag::SUB);
            w.u8(if flags.nuw { 1 } else { 0 });
            w.u8(if flags.nsw { 1 } else { 0 });
            encode_vref(w, lhs);
            encode_vref(w, rhs);
        }
        Mul {
            flags, lhs, rhs, ..
        } => {
            w.u32(instr_tag::MUL);
            w.u8(if flags.nuw { 1 } else { 0 });
            w.u8(if flags.nsw { 1 } else { 0 });
            encode_vref(w, lhs);
            encode_vref(w, rhs);
        }
        UDiv { exact, lhs, rhs } => {
            w.u32(instr_tag::UDIV);
            w.u8(if *exact { 1 } else { 0 });
            encode_vref(w, lhs);
            encode_vref(w, rhs);
        }
        SDiv { exact, lhs, rhs } => {
            w.u32(instr_tag::SDIV);
            w.u8(if *exact { 1 } else { 0 });
            encode_vref(w, lhs);
            encode_vref(w, rhs);
        }
        URem { lhs, rhs } => {
            w.u32(instr_tag::UREM);
            encode_vref(w, lhs);
            encode_vref(w, rhs);
        }
        SRem { lhs, rhs } => {
            w.u32(instr_tag::SREM);
            encode_vref(w, lhs);
            encode_vref(w, rhs);
        }
        And { lhs, rhs } => {
            w.u32(instr_tag::AND);
            encode_vref(w, lhs);
            encode_vref(w, rhs);
        }
        Or { lhs, rhs } => {
            w.u32(instr_tag::OR);
            encode_vref(w, lhs);
            encode_vref(w, rhs);
        }
        Xor { lhs, rhs } => {
            w.u32(instr_tag::XOR);
            encode_vref(w, lhs);
            encode_vref(w, rhs);
        }
        Shl {
            flags, lhs, rhs, ..
        } => {
            w.u32(instr_tag::SHL);
            w.u8(if flags.nuw { 1 } else { 0 });
            w.u8(if flags.nsw { 1 } else { 0 });
            encode_vref(w, lhs);
            encode_vref(w, rhs);
        }
        LShr {
            exact, lhs, rhs, ..
        } => {
            w.u32(instr_tag::LSHR);
            w.u8(if *exact { 1 } else { 0 });
            encode_vref(w, lhs);
            encode_vref(w, rhs);
        }
        AShr {
            exact, lhs, rhs, ..
        } => {
            w.u32(instr_tag::ASHR);
            w.u8(if *exact { 1 } else { 0 });
            encode_vref(w, lhs);
            encode_vref(w, rhs);
        }
        FAdd { lhs, rhs, .. } => {
            w.u32(instr_tag::FADD);
            encode_vref(w, lhs);
            encode_vref(w, rhs);
        }
        FSub { lhs, rhs, .. } => {
            w.u32(instr_tag::FSUB);
            encode_vref(w, lhs);
            encode_vref(w, rhs);
        }
        FMul { lhs, rhs, .. } => {
            w.u32(instr_tag::FMUL);
            encode_vref(w, lhs);
            encode_vref(w, rhs);
        }
        FDiv { lhs, rhs, .. } => {
            w.u32(instr_tag::FDIV);
            encode_vref(w, lhs);
            encode_vref(w, rhs);
        }
        FRem { lhs, rhs, .. } => {
            w.u32(instr_tag::FREM);
            encode_vref(w, lhs);
            encode_vref(w, rhs);
        }
        FNeg { operand, .. } => {
            w.u32(instr_tag::FNEG);
            encode_vref(w, operand);
        }
        ICmp { pred, lhs, rhs } => {
            w.u32(instr_tag::ICMP);
            w.u8(encode_int_pred(*pred));
            encode_vref(w, lhs);
            encode_vref(w, rhs);
        }
        FCmp { pred, lhs, rhs, .. } => {
            w.u32(instr_tag::FCMP);
            w.u8(encode_float_pred(*pred));
            encode_vref(w, lhs);
            encode_vref(w, rhs);
        }
        Alloca {
            alloc_ty,
            num_elements,
            align,
        } => {
            w.u32(instr_tag::ALLOCA);
            w.u32(alloc_ty.0);
            encode_opt_vref(w, num_elements);
            encode_opt_u32(w, *align);
        }
        Load {
            ty,
            ptr,
            align,
            volatile,
        } => {
            w.u32(instr_tag::LOAD);
            w.u32(ty.0);
            encode_vref(w, ptr);
            encode_opt_u32(w, *align);
            w.u8(if *volatile { 1 } else { 0 });
        }
        Store {
            val,
            ptr,
            align,
            volatile,
        } => {
            w.u32(instr_tag::STORE);
            encode_vref(w, val);
            encode_vref(w, ptr);
            encode_opt_u32(w, *align);
            w.u8(if *volatile { 1 } else { 0 });
        }
        GetElementPtr {
            inbounds,
            base_ty,
            ptr,
            indices,
        } => {
            w.u32(instr_tag::GEP);
            w.u8(if *inbounds { 1 } else { 0 });
            w.u32(base_ty.0);
            encode_vref(w, ptr);
            w.u32(indices.len() as u32);
            for idx in indices {
                encode_vref(w, idx);
            }
        }
        Trunc { val, to } => {
            w.u32(instr_tag::TRUNC);
            encode_vref(w, val);
            w.u32(to.0);
        }
        ZExt { val, to } => {
            w.u32(instr_tag::ZEXT);
            encode_vref(w, val);
            w.u32(to.0);
        }
        SExt { val, to } => {
            w.u32(instr_tag::SEXT);
            encode_vref(w, val);
            w.u32(to.0);
        }
        FPTrunc { val, to } => {
            w.u32(instr_tag::FPTRUNC);
            encode_vref(w, val);
            w.u32(to.0);
        }
        FPExt { val, to } => {
            w.u32(instr_tag::FPEXT);
            encode_vref(w, val);
            w.u32(to.0);
        }
        FPToUI { val, to } => {
            w.u32(instr_tag::FPTOUI);
            encode_vref(w, val);
            w.u32(to.0);
        }
        FPToSI { val, to } => {
            w.u32(instr_tag::FPTOSI);
            encode_vref(w, val);
            w.u32(to.0);
        }
        UIToFP { val, to } => {
            w.u32(instr_tag::UITOFP);
            encode_vref(w, val);
            w.u32(to.0);
        }
        SIToFP { val, to } => {
            w.u32(instr_tag::SITOFP);
            encode_vref(w, val);
            w.u32(to.0);
        }
        PtrToInt { val, to } => {
            w.u32(instr_tag::PTRTOINT);
            encode_vref(w, val);
            w.u32(to.0);
        }
        IntToPtr { val, to } => {
            w.u32(instr_tag::INTTOPTR);
            encode_vref(w, val);
            w.u32(to.0);
        }
        BitCast { val, to } => {
            w.u32(instr_tag::BITCAST);
            encode_vref(w, val);
            w.u32(to.0);
        }
        AddrSpaceCast { val, to } => {
            w.u32(instr_tag::ADDRSPACECAST);
            encode_vref(w, val);
            w.u32(to.0);
        }
        Select {
            cond,
            then_val,
            else_val,
        } => {
            w.u32(instr_tag::SELECT);
            encode_vref(w, cond);
            encode_vref(w, then_val);
            encode_vref(w, else_val);
        }
        Phi { ty, incoming } => {
            w.u32(instr_tag::PHI);
            w.u32(ty.0);
            w.u32(incoming.len() as u32);
            for (vr, bid) in incoming {
                encode_vref(w, vr);
                w.u32(bid.0);
            }
        }
        ExtractValue { aggregate, indices } => {
            w.u32(instr_tag::EXTRACTVALUE);
            encode_vref(w, aggregate);
            w.u32(indices.len() as u32);
            for &i in indices {
                w.u32(i);
            }
        }
        InsertValue {
            aggregate,
            val,
            indices,
        } => {
            w.u32(instr_tag::INSERTVALUE);
            encode_vref(w, aggregate);
            encode_vref(w, val);
            w.u32(indices.len() as u32);
            for &i in indices {
                w.u32(i);
            }
        }
        ExtractElement { vec, idx } => {
            w.u32(instr_tag::EXTRACTELEM);
            encode_vref(w, vec);
            encode_vref(w, idx);
        }
        InsertElement { vec, val, idx } => {
            w.u32(instr_tag::INSERTELEM);
            encode_vref(w, vec);
            encode_vref(w, val);
            encode_vref(w, idx);
        }
        ShuffleVector { v1, v2, mask } => {
            w.u32(instr_tag::SHUFFLEVEC);
            encode_vref(w, v1);
            encode_vref(w, v2);
            w.u32(mask.len() as u32);
            for &m in mask {
                w.i32(m);
            }
        }
        Call {
            tail,
            callee_ty,
            callee,
            args,
        } => {
            w.u32(instr_tag::CALL);
            use llvm_ir::TailCallKind;
            let tail_tag = match tail {
                TailCallKind::None => 0u8,
                TailCallKind::Tail => 1,
                TailCallKind::MustTail => 2,
                TailCallKind::NoTail => 3,
            };
            w.u8(tail_tag);
            w.u32(callee_ty.0);
            encode_vref(w, callee);
            w.u32(args.len() as u32);
            for arg in args {
                encode_vref(w, arg);
            }
        }
        Ret { val } => {
            w.u32(instr_tag::RET);
            encode_opt_vref(w, val);
        }
        Br { dest } => {
            w.u32(instr_tag::BR);
            w.u32(dest.0);
        }
        CondBr {
            cond,
            then_dest,
            else_dest,
        } => {
            w.u32(instr_tag::CONDBR);
            encode_vref(w, cond);
            w.u32(then_dest.0);
            w.u32(else_dest.0);
        }
        Switch {
            val,
            default,
            cases,
        } => {
            w.u32(instr_tag::SWITCH);
            encode_vref(w, val);
            w.u32(default.0);
            w.u32(cases.len() as u32);
            for (cv, bd) in cases {
                encode_vref(w, cv);
                w.u32(bd.0);
            }
        }
        Unreachable => {
            w.u32(instr_tag::UNREACHABLE);
        }
    }
}

fn encode_opt_u32(w: &mut Writer, v: Option<u32>) {
    match v {
        Some(x) => {
            w.u8(1);
            w.u32(x);
        }
        None => {
            w.u8(0);
        }
    }
}

fn encode_int_pred(pred: llvm_ir::IntPredicate) -> u8 {
    use llvm_ir::IntPredicate::*;
    match pred {
        Eq => 0,
        Ne => 1,
        Ugt => 2,
        Uge => 3,
        Ult => 4,
        Ule => 5,
        Sgt => 6,
        Sge => 7,
        Slt => 8,
        Sle => 9,
    }
}

fn encode_float_pred(pred: llvm_ir::FloatPredicate) -> u8 {
    use llvm_ir::FloatPredicate::*;
    match pred {
        False => 0,
        Oeq => 1,
        Ogt => 2,
        Oge => 3,
        Olt => 4,
        Ole => 5,
        One => 6,
        Ord => 7,
        Uno => 8,
        Ueq => 9,
        Ugt => 10,
        Uge => 11,
        Ult => 12,
        Ule => 13,
        Une => 14,
        True => 15,
    }
}

// ── writer helper ─────────────────────────────────────────────────────────

#[derive(Default)]
struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    fn raw(&mut self, b: &[u8]) {
        self.buf.extend_from_slice(b);
    }
    fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }
    fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn i32(&mut self, v: i32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    /// Length-prefixed UTF-8 string.  A length of 0 means "absent/empty".
    fn string(&mut self, s: &str) {
        self.u32(s.len() as u32);
        self.raw(s.as_bytes());
    }
}

// ── public API re-exported from crate root ────────────────────────────────

pub use write_bitcode as write;
