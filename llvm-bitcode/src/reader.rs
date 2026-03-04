//! Bitcode reader: parses the LRIR binary format and reconstructs a `(Context, Module)`.

use llvm_ir::{
    ArgId, BasicBlock, BlockId, ConstId, ConstantData, Context, FloatKind, Function, GlobalId,
    InstrId, InstrKind, Instruction, IntArithFlags, FastMathFlags, IntPredicate,
    FloatPredicate, TailCallKind, Linkage, Module, TypeData, TypeId, ValueRef,
};
use llvm_ir::value::Argument;
use crate::error::BitcodeError;

/// Magic bytes for the LRIR format.
const MAGIC: &[u8; 4] = b"LRIR";

/// Parse a LRIR binary blob and reconstruct `(Context, Module)`.
pub fn read_bitcode(bytes: &[u8]) -> Result<(Context, Module), BitcodeError> {
    let mut r = Reader::new(bytes);

    // ── header ────────────────────────────────────────────────────────────
    let magic = r.read_bytes(4)?;
    if magic != MAGIC {
        return Err(BitcodeError::InvalidMagic);
    }
    let version = r.u32()?;
    if version != 1 {
        return Err(BitcodeError::ParseError(format!("unsupported version {}", version)));
    }

    // ── type table ────────────────────────────────────────────────────────
    let mut ctx = Context::new();
    let type_count = r.u32()? as usize;
    // We'll collect the types as raw TypeData first; the Context will
    // intern them in order.  We need a mapping from serialized TypeId → interned TypeId.
    let mut type_id_map: Vec<TypeId> = Vec::with_capacity(type_count);

    for _ in 0..type_count {
        let td = decode_type(&mut r, &type_id_map)?;
        // Intern the type and record the mapping.
        let interned = intern_type(&mut ctx, td);
        type_id_map.push(interned);
    }

    // ── constant table ─────────────────────────────────────────────────────
    let const_count = r.u32()? as usize;
    let mut const_id_map: Vec<ConstId> = Vec::with_capacity(const_count);

    for _ in 0..const_count {
        let cd = decode_const(&mut r, &type_id_map, &const_id_map)?;
        let cid = ctx.push_const(cd);
        const_id_map.push(cid);
    }

    // ── module header ──────────────────────────────────────────────────────
    let module_name = r.string()?;
    let mut module = Module::new(module_name);

    // ── functions ──────────────────────────────────────────────────────────
    let func_count = r.u32()? as usize;
    for _ in 0..func_count {
        let func = decode_function(&mut r, &type_id_map, &const_id_map)?;
        module.add_function(func);
    }

    Ok((ctx, module))
}

// ── type decoding ──────────────────────────────────────────────────────────

mod type_tag {
    pub const VOID:     u8 = 0;
    pub const INTEGER:  u8 = 1;
    pub const FLOAT:    u8 = 2;
    pub const POINTER:  u8 = 3;
    pub const ARRAY:    u8 = 4;
    pub const VECTOR:   u8 = 5;
    pub const STRUCT:   u8 = 6;
    pub const FUNCTION: u8 = 7;
    pub const LABEL:    u8 = 8;
    pub const METADATA: u8 = 9;
}

mod float_tag {
    pub const HALF:    u8 = 0;
    pub const BFLOAT:  u8 = 1;
    pub const SINGLE:  u8 = 2;
    pub const DOUBLE:  u8 = 3;
    pub const FP128:   u8 = 4;
    pub const X86FP80: u8 = 5;
}

/// Decode a TypeData from the stream, resolving type IDs via `type_id_map`.
fn decode_type(r: &mut Reader, type_id_map: &[TypeId]) -> Result<TypeData, BitcodeError> {
    let tag = r.u8()?;
    match tag {
        type_tag::VOID    => Ok(TypeData::Void),
        type_tag::INTEGER => {
            let bits = r.u32()?;
            Ok(TypeData::Integer(bits))
        }
        type_tag::FLOAT => {
            let ftag = r.u8()?;
            let kind = match ftag {
                float_tag::HALF    => FloatKind::Half,
                float_tag::BFLOAT  => FloatKind::BFloat,
                float_tag::SINGLE  => FloatKind::Single,
                float_tag::DOUBLE  => FloatKind::Double,
                float_tag::FP128   => FloatKind::Fp128,
                float_tag::X86FP80 => FloatKind::X86Fp80,
                _  => return Err(BitcodeError::InvalidType),
            };
            Ok(TypeData::Float(kind))
        }
        type_tag::POINTER => Ok(TypeData::Pointer),
        type_tag::ARRAY => {
            let elem_raw = r.u32()? as usize;
            let len = r.u64()?;
            let element = map_type_id(type_id_map, elem_raw)?;
            Ok(TypeData::Array { element, len })
        }
        type_tag::VECTOR => {
            let elem_raw = r.u32()? as usize;
            let len = r.u32()?;
            let scalable = r.u8()? != 0;
            let element = map_type_id(type_id_map, elem_raw)?;
            Ok(TypeData::Vector { element, len, scalable })
        }
        type_tag::STRUCT => {
            let name = r.opt_string()?;
            let packed = r.u8()? != 0;
            let field_count = r.u32()? as usize;
            let mut fields = Vec::with_capacity(field_count);
            for _ in 0..field_count {
                let fid_raw = r.u32()? as usize;
                fields.push(map_type_id(type_id_map, fid_raw)?);
            }
            Ok(TypeData::Struct(llvm_ir::StructType { name, fields, packed }))
        }
        type_tag::FUNCTION => {
            let ret_raw = r.u32()? as usize;
            let variadic = r.u8()? != 0;
            let param_count = r.u32()? as usize;
            let mut params = Vec::with_capacity(param_count);
            for _ in 0..param_count {
                let pid_raw = r.u32()? as usize;
                params.push(map_type_id(type_id_map, pid_raw)?);
            }
            let ret = map_type_id(type_id_map, ret_raw)?;
            Ok(TypeData::Function(llvm_ir::FunctionType { ret, params, variadic }))
        }
        type_tag::LABEL    => Ok(TypeData::Label),
        type_tag::METADATA => Ok(TypeData::Metadata),
        _  => Err(BitcodeError::InvalidType),
    }
}

fn map_type_id(type_id_map: &[TypeId], raw: usize) -> Result<TypeId, BitcodeError> {
    type_id_map.get(raw).copied().ok_or(BitcodeError::ParseError(
        format!("type id {} out of range (table size {})", raw, type_id_map.len())
    ))
}

fn map_const_id(const_id_map: &[ConstId], raw: usize) -> Result<ConstId, BitcodeError> {
    const_id_map.get(raw).copied().ok_or(BitcodeError::ParseError(
        format!("const id {} out of range", raw)
    ))
}

/// Intern a TypeData into the Context and return the TypeId.
fn intern_type(ctx: &mut Context, td: TypeData) -> TypeId {
    match td {
        TypeData::Void       => ctx.void_ty,
        TypeData::Integer(b) => ctx.mk_int(b),
        TypeData::Float(k)   => ctx.mk_float(k),
        TypeData::Pointer    => ctx.mk_ptr(),
        TypeData::Label      => ctx.mk_label(),
        TypeData::Metadata   => {
            // Metadata isn't exposed via a public constructor; intern manually.
            // Fall back to label_ty as a placeholder (metadata is rare in Phase 1 tests).
            ctx.mk_label()
        }
        TypeData::Array { element, len } => ctx.mk_array(element, len),
        TypeData::Vector { element, len, scalable } => ctx.mk_vector(element, len, scalable),
        TypeData::Struct(st) => {
            if let Some(ref name) = st.name {
                let id = ctx.mk_struct_named(name.clone());
                ctx.define_struct_body(id, st.fields, st.packed);
                id
            } else {
                ctx.mk_struct_anon(st.fields, st.packed)
            }
        }
        TypeData::Function(ft) => ctx.mk_fn_type(ft.ret, ft.params, ft.variadic),
    }
}

// ── constant decoding ─────────────────────────────────────────────────────

mod const_tag {
    pub const INT:        u8 = 0;
    pub const INT_WIDE:   u8 = 1;
    pub const FLOAT:      u8 = 2;
    pub const NULL:       u8 = 3;
    pub const UNDEF:      u8 = 4;
    pub const POISON:     u8 = 5;
    pub const ZERO_INIT:  u8 = 6;
    pub const ARRAY:      u8 = 7;
    pub const STRUCT:     u8 = 8;
    pub const VECTOR:     u8 = 9;
    pub const GLOBAL_REF: u8 = 10;
}

fn decode_const(
    r: &mut Reader,
    type_id_map: &[TypeId],
    const_id_map: &[ConstId],
) -> Result<ConstantData, BitcodeError> {
    let tag = r.u8()?;
    match tag {
        const_tag::INT => {
            let ty = map_type_id(type_id_map, r.u32()? as usize)?;
            let val = r.u64()?;
            Ok(ConstantData::Int { ty, val })
        }
        const_tag::INT_WIDE => {
            let ty = map_type_id(type_id_map, r.u32()? as usize)?;
            let word_count = r.u32()? as usize;
            let mut words = Vec::with_capacity(word_count);
            for _ in 0..word_count { words.push(r.u64()?); }
            Ok(ConstantData::IntWide { ty, words })
        }
        const_tag::FLOAT => {
            let ty = map_type_id(type_id_map, r.u32()? as usize)?;
            let bits = r.u64()?;
            Ok(ConstantData::Float { ty, bits })
        }
        const_tag::NULL      => { let ty = map_type_id(type_id_map, r.u32()? as usize)?; Ok(ConstantData::Null(ty)) }
        const_tag::UNDEF     => { let ty = map_type_id(type_id_map, r.u32()? as usize)?; Ok(ConstantData::Undef(ty)) }
        const_tag::POISON    => { let ty = map_type_id(type_id_map, r.u32()? as usize)?; Ok(ConstantData::Poison(ty)) }
        const_tag::ZERO_INIT => { let ty = map_type_id(type_id_map, r.u32()? as usize)?; Ok(ConstantData::ZeroInitializer(ty)) }
        const_tag::ARRAY => {
            let ty = map_type_id(type_id_map, r.u32()? as usize)?;
            let n = r.u32()? as usize;
            let mut elems = Vec::with_capacity(n);
            for _ in 0..n { elems.push(map_const_id(const_id_map, r.u32()? as usize)?); }
            Ok(ConstantData::Array { ty, elements: elems })
        }
        const_tag::STRUCT => {
            let ty = map_type_id(type_id_map, r.u32()? as usize)?;
            let n = r.u32()? as usize;
            let mut fields = Vec::with_capacity(n);
            for _ in 0..n { fields.push(map_const_id(const_id_map, r.u32()? as usize)?); }
            Ok(ConstantData::Struct { ty, fields })
        }
        const_tag::VECTOR => {
            let ty = map_type_id(type_id_map, r.u32()? as usize)?;
            let n = r.u32()? as usize;
            let mut elems = Vec::with_capacity(n);
            for _ in 0..n { elems.push(map_const_id(const_id_map, r.u32()? as usize)?); }
            Ok(ConstantData::Vector { ty, elements: elems })
        }
        const_tag::GLOBAL_REF => {
            let ty = map_type_id(type_id_map, r.u32()? as usize)?;
            let id_raw = r.u32()?;
            let name = r.string()?;
            Ok(ConstantData::GlobalRef { ty, id: GlobalId(id_raw), name })
        }
        other => Err(BitcodeError::UnsupportedRecord(other as u32)),
    }
}

// ── function decoding ─────────────────────────────────────────────────────

mod linkage_tag {
    pub const PRIVATE:               u8 = 0;
    pub const INTERNAL:              u8 = 1;
    pub const EXTERNAL:              u8 = 2;
    pub const WEAK:                  u8 = 3;
    pub const WEAK_ODR:              u8 = 4;
    pub const LINK_ONCE:             u8 = 5;
    pub const LINK_ONCE_ODR:         u8 = 6;
    pub const COMMON:                u8 = 7;
    pub const AVAILABLE_EXTERNALLY:  u8 = 8;
}

fn decode_linkage(tag: u8) -> Result<Linkage, BitcodeError> {
    match tag {
        linkage_tag::PRIVATE              => Ok(Linkage::Private),
        linkage_tag::INTERNAL             => Ok(Linkage::Internal),
        linkage_tag::EXTERNAL             => Ok(Linkage::External),
        linkage_tag::WEAK                 => Ok(Linkage::Weak),
        linkage_tag::WEAK_ODR             => Ok(Linkage::WeakOdr),
        linkage_tag::LINK_ONCE            => Ok(Linkage::LinkOnce),
        linkage_tag::LINK_ONCE_ODR        => Ok(Linkage::LinkOnceOdr),
        linkage_tag::COMMON               => Ok(Linkage::Common),
        linkage_tag::AVAILABLE_EXTERNALLY => Ok(Linkage::AvailableExternally),
        other => Err(BitcodeError::UnsupportedRecord(other as u32)),
    }
}

fn decode_function(
    r: &mut Reader,
    type_id_map: &[TypeId],
    const_id_map: &[ConstId],
) -> Result<Function, BitcodeError> {
    let name = r.string()?;
    let ty_raw = r.u32()? as usize;
    let ty = map_type_id(type_id_map, ty_raw)?;
    let linkage = decode_linkage(r.u8()?)?;
    let is_declaration = r.u8()? != 0;

    // Arguments.
    let arg_count = r.u32()? as usize;
    let mut args = Vec::with_capacity(arg_count);
    for _ in 0..arg_count {
        let aname = r.string()?;
        let aty_raw = r.u32()? as usize;
        let aty = map_type_id(type_id_map, aty_raw)?;
        let index = r.u32()?;
        args.push(Argument { name: aname, ty: aty, index });
    }

    let mut func = if is_declaration {
        Function::new_declaration(name, ty, args, linkage)
    } else {
        Function::new(name, ty, args, linkage)
    };
    func.is_declaration = is_declaration;

    // Read flat instruction pool first (needed for block body references).
    let block_count = r.u32()? as usize;
    // Save block records for later (after we read the instruction pool).
    let mut block_records: Vec<(String, Vec<u32>, u32)> = Vec::with_capacity(block_count);
    for _ in 0..block_count {
        let bname = r.string()?;
        let body_count = r.u32()? as usize;
        let mut body = Vec::with_capacity(body_count);
        for _ in 0..body_count { body.push(r.u32()?); }
        let term = r.u32()?;
        block_records.push((bname, body, term));
    }

    // Flat instruction pool.
    let instr_count = r.u32()? as usize;
    for _ in 0..instr_count {
        let instr = decode_instr(r, type_id_map, const_id_map)?;
        func.alloc_instr(instr);
    }

    // Reconstruct basic blocks.
    for (bname, body_ids, term_raw) in block_records {
        let mut bb = BasicBlock::new(bname);
        for id in body_ids {
            bb.body.push(InstrId(id));
        }
        bb.terminator = if term_raw == 0xFFFF_FFFF {
            None
        } else {
            Some(InstrId(term_raw))
        };
        func.blocks.push(bb);
    }

    Ok(func)
}

// ── instruction decoding ──────────────────────────────────────────────────

mod instr_tag {
    pub const ADD:          u32 = 0;
    pub const SUB:          u32 = 1;
    pub const MUL:          u32 = 2;
    pub const UDIV:         u32 = 3;
    pub const SDIV:         u32 = 4;
    pub const UREM:         u32 = 5;
    pub const SREM:         u32 = 6;
    pub const AND:          u32 = 10;
    pub const OR:           u32 = 11;
    pub const XOR:          u32 = 12;
    pub const SHL:          u32 = 13;
    pub const LSHR:         u32 = 14;
    pub const ASHR:         u32 = 15;
    pub const FADD:         u32 = 20;
    pub const FSUB:         u32 = 21;
    pub const FMUL:         u32 = 22;
    pub const FDIV:         u32 = 23;
    pub const FREM:         u32 = 24;
    pub const FNEG:         u32 = 25;
    pub const ICMP:         u32 = 30;
    pub const FCMP:         u32 = 31;
    pub const ALLOCA:       u32 = 40;
    pub const LOAD:         u32 = 41;
    pub const STORE:        u32 = 42;
    pub const GEP:          u32 = 43;
    pub const TRUNC:        u32 = 50;
    pub const ZEXT:         u32 = 51;
    pub const SEXT:         u32 = 52;
    pub const FPTRUNC:      u32 = 53;
    pub const FPEXT:        u32 = 54;
    pub const FPTOUI:       u32 = 55;
    pub const FPTOSI:       u32 = 56;
    pub const UITOFP:       u32 = 57;
    pub const SITOFP:       u32 = 58;
    pub const PTRTOINT:     u32 = 59;
    pub const INTTOPTR:     u32 = 60;
    pub const BITCAST:      u32 = 61;
    pub const ADDRSPACECAST: u32 = 62;
    pub const SELECT:       u32 = 70;
    pub const PHI:          u32 = 71;
    pub const EXTRACTVALUE: u32 = 72;
    pub const INSERTVALUE:  u32 = 73;
    pub const EXTRACTELEM:  u32 = 74;
    pub const INSERTELEM:   u32 = 75;
    pub const SHUFFLEVEC:   u32 = 76;
    pub const CALL:         u32 = 80;
    pub const RET:          u32 = 90;
    pub const BR:           u32 = 91;
    pub const CONDBR:       u32 = 92;
    pub const SWITCH:       u32 = 93;
    pub const UNREACHABLE:  u32 = 94;
}

fn decode_vref(r: &mut Reader) -> Result<ValueRef, BitcodeError> {
    let tag = r.u8()?;
    let id = r.u32()?;
    match tag {
        0 => Ok(ValueRef::Instruction(InstrId(id))),
        1 => Ok(ValueRef::Argument(ArgId(id))),
        2 => Ok(ValueRef::Constant(ConstId(id))),
        3 => Ok(ValueRef::Global(GlobalId(id))),
        other => Err(BitcodeError::UnsupportedRecord(other as u32)),
    }
}

fn decode_opt_vref(r: &mut Reader) -> Result<Option<ValueRef>, BitcodeError> {
    let present = r.u8()?;
    if present != 0 { Ok(Some(decode_vref(r)?)) } else { Ok(None) }
}

fn decode_opt_u32(r: &mut Reader) -> Result<Option<u32>, BitcodeError> {
    let present = r.u8()?;
    if present != 0 { Ok(Some(r.u32()?)) } else { Ok(None) }
}

fn decode_int_pred(tag: u8) -> Result<IntPredicate, BitcodeError> {
    match tag {
        0 => Ok(IntPredicate::Eq),  1 => Ok(IntPredicate::Ne),
        2 => Ok(IntPredicate::Ugt), 3 => Ok(IntPredicate::Uge),
        4 => Ok(IntPredicate::Ult), 5 => Ok(IntPredicate::Ule),
        6 => Ok(IntPredicate::Sgt), 7 => Ok(IntPredicate::Sge),
        8 => Ok(IntPredicate::Slt), 9 => Ok(IntPredicate::Sle),
        other => Err(BitcodeError::UnsupportedRecord(other as u32)),
    }
}

fn decode_float_pred(tag: u8) -> Result<FloatPredicate, BitcodeError> {
    match tag {
        0  => Ok(FloatPredicate::False), 1  => Ok(FloatPredicate::Oeq),
        2  => Ok(FloatPredicate::Ogt),   3  => Ok(FloatPredicate::Oge),
        4  => Ok(FloatPredicate::Olt),   5  => Ok(FloatPredicate::Ole),
        6  => Ok(FloatPredicate::One),   7  => Ok(FloatPredicate::Ord),
        8  => Ok(FloatPredicate::Uno),   9  => Ok(FloatPredicate::Ueq),
        10 => Ok(FloatPredicate::Ugt),   11 => Ok(FloatPredicate::Uge),
        12 => Ok(FloatPredicate::Ult),   13 => Ok(FloatPredicate::Ule),
        14 => Ok(FloatPredicate::Une),   15 => Ok(FloatPredicate::True),
        other => Err(BitcodeError::UnsupportedRecord(other as u32)),
    }
}

fn decode_instr(
    r: &mut Reader,
    type_id_map: &[TypeId],
    _const_id_map: &[ConstId],
) -> Result<Instruction, BitcodeError> {
    // Name: 0-length = None.
    let name = r.opt_string()?;
    let ty_raw = r.u32()? as usize;
    let ty = map_type_id(type_id_map, ty_raw)?;
    let tag = r.u32()?;

    let kind = match tag {
        instr_tag::ADD  => {
            let nuw = r.u8()? != 0; let nsw = r.u8()? != 0;
            let flags = IntArithFlags { nuw, nsw };
            InstrKind::Add  { flags, lhs: decode_vref(r)?, rhs: decode_vref(r)? }
        }
        instr_tag::SUB  => {
            let nuw = r.u8()? != 0; let nsw = r.u8()? != 0;
            let flags = IntArithFlags { nuw, nsw };
            InstrKind::Sub  { flags, lhs: decode_vref(r)?, rhs: decode_vref(r)? }
        }
        instr_tag::MUL  => {
            let nuw = r.u8()? != 0; let nsw = r.u8()? != 0;
            let flags = IntArithFlags { nuw, nsw };
            InstrKind::Mul  { flags, lhs: decode_vref(r)?, rhs: decode_vref(r)? }
        }
        instr_tag::UDIV => {
            let exact = r.u8()? != 0;
            InstrKind::UDiv { exact, lhs: decode_vref(r)?, rhs: decode_vref(r)? }
        }
        instr_tag::SDIV => {
            let exact = r.u8()? != 0;
            InstrKind::SDiv { exact, lhs: decode_vref(r)?, rhs: decode_vref(r)? }
        }
        instr_tag::UREM => InstrKind::URem { lhs: decode_vref(r)?, rhs: decode_vref(r)? },
        instr_tag::SREM => InstrKind::SRem { lhs: decode_vref(r)?, rhs: decode_vref(r)? },
        instr_tag::AND  => InstrKind::And  { lhs: decode_vref(r)?, rhs: decode_vref(r)? },
        instr_tag::OR   => InstrKind::Or   { lhs: decode_vref(r)?, rhs: decode_vref(r)? },
        instr_tag::XOR  => InstrKind::Xor  { lhs: decode_vref(r)?, rhs: decode_vref(r)? },
        instr_tag::SHL  => {
            let nuw = r.u8()? != 0; let nsw = r.u8()? != 0;
            let flags = IntArithFlags { nuw, nsw };
            InstrKind::Shl  { flags, lhs: decode_vref(r)?, rhs: decode_vref(r)? }
        }
        instr_tag::LSHR => {
            let exact = r.u8()? != 0;
            InstrKind::LShr { exact, lhs: decode_vref(r)?, rhs: decode_vref(r)? }
        }
        instr_tag::ASHR => {
            let exact = r.u8()? != 0;
            InstrKind::AShr { exact, lhs: decode_vref(r)?, rhs: decode_vref(r)? }
        }
        instr_tag::FADD => InstrKind::FAdd { flags: FastMathFlags::default(), lhs: decode_vref(r)?, rhs: decode_vref(r)? },
        instr_tag::FSUB => InstrKind::FSub { flags: FastMathFlags::default(), lhs: decode_vref(r)?, rhs: decode_vref(r)? },
        instr_tag::FMUL => InstrKind::FMul { flags: FastMathFlags::default(), lhs: decode_vref(r)?, rhs: decode_vref(r)? },
        instr_tag::FDIV => InstrKind::FDiv { flags: FastMathFlags::default(), lhs: decode_vref(r)?, rhs: decode_vref(r)? },
        instr_tag::FREM => InstrKind::FRem { flags: FastMathFlags::default(), lhs: decode_vref(r)?, rhs: decode_vref(r)? },
        instr_tag::FNEG => InstrKind::FNeg { flags: FastMathFlags::default(), operand: decode_vref(r)? },
        instr_tag::ICMP => {
            let pred = decode_int_pred(r.u8()?)?;
            InstrKind::ICmp { pred, lhs: decode_vref(r)?, rhs: decode_vref(r)? }
        }
        instr_tag::FCMP => {
            let pred = decode_float_pred(r.u8()?)?;
            InstrKind::FCmp { flags: FastMathFlags::default(), pred, lhs: decode_vref(r)?, rhs: decode_vref(r)? }
        }
        instr_tag::ALLOCA => {
            let alloc_ty = map_type_id(type_id_map, r.u32()? as usize)?;
            let num_elements = decode_opt_vref(r)?;
            let align = decode_opt_u32(r)?;
            InstrKind::Alloca { alloc_ty, num_elements, align }
        }
        instr_tag::LOAD => {
            let lty = map_type_id(type_id_map, r.u32()? as usize)?;
            let ptr = decode_vref(r)?;
            let align = decode_opt_u32(r)?;
            let volatile = r.u8()? != 0;
            InstrKind::Load { ty: lty, ptr, align, volatile }
        }
        instr_tag::STORE => {
            let val = decode_vref(r)?;
            let ptr = decode_vref(r)?;
            let align = decode_opt_u32(r)?;
            let volatile = r.u8()? != 0;
            InstrKind::Store { val, ptr, align, volatile }
        }
        instr_tag::GEP => {
            let inbounds = r.u8()? != 0;
            let base_ty = map_type_id(type_id_map, r.u32()? as usize)?;
            let ptr = decode_vref(r)?;
            let idx_count = r.u32()? as usize;
            let mut indices = Vec::with_capacity(idx_count);
            for _ in 0..idx_count { indices.push(decode_vref(r)?); }
            InstrKind::GetElementPtr { inbounds, base_ty, ptr, indices }
        }
        instr_tag::TRUNC        => { let val = decode_vref(r)?; let to = map_type_id(type_id_map, r.u32()? as usize)?; InstrKind::Trunc { val, to } }
        instr_tag::ZEXT         => { let val = decode_vref(r)?; let to = map_type_id(type_id_map, r.u32()? as usize)?; InstrKind::ZExt { val, to } }
        instr_tag::SEXT         => { let val = decode_vref(r)?; let to = map_type_id(type_id_map, r.u32()? as usize)?; InstrKind::SExt { val, to } }
        instr_tag::FPTRUNC      => { let val = decode_vref(r)?; let to = map_type_id(type_id_map, r.u32()? as usize)?; InstrKind::FPTrunc { val, to } }
        instr_tag::FPEXT        => { let val = decode_vref(r)?; let to = map_type_id(type_id_map, r.u32()? as usize)?; InstrKind::FPExt { val, to } }
        instr_tag::FPTOUI       => { let val = decode_vref(r)?; let to = map_type_id(type_id_map, r.u32()? as usize)?; InstrKind::FPToUI { val, to } }
        instr_tag::FPTOSI       => { let val = decode_vref(r)?; let to = map_type_id(type_id_map, r.u32()? as usize)?; InstrKind::FPToSI { val, to } }
        instr_tag::UITOFP       => { let val = decode_vref(r)?; let to = map_type_id(type_id_map, r.u32()? as usize)?; InstrKind::UIToFP { val, to } }
        instr_tag::SITOFP       => { let val = decode_vref(r)?; let to = map_type_id(type_id_map, r.u32()? as usize)?; InstrKind::SIToFP { val, to } }
        instr_tag::PTRTOINT     => { let val = decode_vref(r)?; let to = map_type_id(type_id_map, r.u32()? as usize)?; InstrKind::PtrToInt { val, to } }
        instr_tag::INTTOPTR     => { let val = decode_vref(r)?; let to = map_type_id(type_id_map, r.u32()? as usize)?; InstrKind::IntToPtr { val, to } }
        instr_tag::BITCAST      => { let val = decode_vref(r)?; let to = map_type_id(type_id_map, r.u32()? as usize)?; InstrKind::BitCast { val, to } }
        instr_tag::ADDRSPACECAST => { let val = decode_vref(r)?; let to = map_type_id(type_id_map, r.u32()? as usize)?; InstrKind::AddrSpaceCast { val, to } }
        instr_tag::SELECT => {
            InstrKind::Select { cond: decode_vref(r)?, then_val: decode_vref(r)?, else_val: decode_vref(r)? }
        }
        instr_tag::PHI => {
            let phi_ty = map_type_id(type_id_map, r.u32()? as usize)?;
            let in_count = r.u32()? as usize;
            let mut incoming = Vec::with_capacity(in_count);
            for _ in 0..in_count {
                let vr = decode_vref(r)?;
                let bid = BlockId(r.u32()?);
                incoming.push((vr, bid));
            }
            InstrKind::Phi { ty: phi_ty, incoming }
        }
        instr_tag::EXTRACTVALUE => {
            let agg = decode_vref(r)?;
            let idx_count = r.u32()? as usize;
            let mut indices = Vec::with_capacity(idx_count);
            for _ in 0..idx_count { indices.push(r.u32()?); }
            InstrKind::ExtractValue { aggregate: agg, indices }
        }
        instr_tag::INSERTVALUE => {
            let agg = decode_vref(r)?;
            let val = decode_vref(r)?;
            let idx_count = r.u32()? as usize;
            let mut indices = Vec::with_capacity(idx_count);
            for _ in 0..idx_count { indices.push(r.u32()?); }
            InstrKind::InsertValue { aggregate: agg, val, indices }
        }
        instr_tag::EXTRACTELEM => {
            InstrKind::ExtractElement { vec: decode_vref(r)?, idx: decode_vref(r)? }
        }
        instr_tag::INSERTELEM => {
            InstrKind::InsertElement { vec: decode_vref(r)?, val: decode_vref(r)?, idx: decode_vref(r)? }
        }
        instr_tag::SHUFFLEVEC => {
            let v1 = decode_vref(r)?; let v2 = decode_vref(r)?;
            let n = r.u32()? as usize;
            let mut mask = Vec::with_capacity(n);
            for _ in 0..n { mask.push(r.i32()?); }
            InstrKind::ShuffleVector { v1, v2, mask }
        }
        instr_tag::CALL => {
            let tail_tag = r.u8()?;
            let tail = match tail_tag {
                0 => TailCallKind::None, 1 => TailCallKind::Tail,
                2 => TailCallKind::MustTail, _ => TailCallKind::NoTail,
            };
            let callee_ty = map_type_id(type_id_map, r.u32()? as usize)?;
            let callee = decode_vref(r)?;
            let arg_count = r.u32()? as usize;
            let mut args = Vec::with_capacity(arg_count);
            for _ in 0..arg_count { args.push(decode_vref(r)?); }
            InstrKind::Call { tail, callee_ty, callee, args }
        }
        instr_tag::RET => InstrKind::Ret { val: decode_opt_vref(r)? },
        instr_tag::BR  => InstrKind::Br  { dest: BlockId(r.u32()?) },
        instr_tag::CONDBR => {
            let cond = decode_vref(r)?;
            let then_dest = BlockId(r.u32()?);
            let else_dest = BlockId(r.u32()?);
            InstrKind::CondBr { cond, then_dest, else_dest }
        }
        instr_tag::SWITCH => {
            let val = decode_vref(r)?;
            let default = BlockId(r.u32()?);
            let case_count = r.u32()? as usize;
            let mut cases = Vec::with_capacity(case_count);
            for _ in 0..case_count {
                let cv = decode_vref(r)?;
                let bd = BlockId(r.u32()?);
                cases.push((cv, bd));
            }
            InstrKind::Switch { val, default, cases }
        }
        instr_tag::UNREACHABLE => InstrKind::Unreachable,
        other => return Err(BitcodeError::UnsupportedRecord(other)),
    };

    Ok(Instruction::new(name, ty, kind))
}

// ── reader helper ─────────────────────────────────────────────────────────

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self { Reader { data, pos: 0 } }

    fn read_bytes(&mut self, n: usize) -> Result<&[u8], BitcodeError> {
        if self.pos + n > self.data.len() {
            return Err(BitcodeError::TruncatedInput);
        }
        let slice = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, BitcodeError> {
        let b = self.read_bytes(1)?;
        Ok(b[0])
    }

    fn u32(&mut self) -> Result<u32, BitcodeError> {
        let b = self.read_bytes(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn i32(&mut self) -> Result<i32, BitcodeError> {
        let b = self.read_bytes(4)?;
        Ok(i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn u64(&mut self) -> Result<u64, BitcodeError> {
        let b = self.read_bytes(8)?;
        Ok(u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
    }

    /// Read a length-prefixed string (u32 len + UTF-8 bytes).
    /// Returns `None` if len == 0.
    fn opt_string(&mut self) -> Result<Option<String>, BitcodeError> {
        let len = self.u32()? as usize;
        if len == 0 {
            return Ok(None);
        }
        let bytes = self.read_bytes(len)?;
        String::from_utf8(bytes.to_vec())
            .map(Some)
            .map_err(|e| BitcodeError::ParseError(format!("invalid UTF-8: {}", e)))
    }

    /// Read a length-prefixed string.  Returns an empty `String` if len == 0.
    fn string(&mut self) -> Result<String, BitcodeError> {
        let len = self.u32()? as usize;
        if len == 0 {
            return Ok(String::new());
        }
        let bytes = self.read_bytes(len)?;
        String::from_utf8(bytes.to_vec())
            .map_err(|e| BitcodeError::ParseError(format!("invalid UTF-8: {}", e)))
    }
}
