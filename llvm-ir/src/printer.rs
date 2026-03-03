//! LLVM IR text format printer (`.ll` file emitter).

use std::fmt::Write as FmtWrite;
use crate::context::{Context, TypeId, ConstId, InstrId, BlockId, ValueRef};
use crate::types::{TypeData, FloatKind, StructType};
use crate::value::ConstantData;
use crate::instruction::{InstrKind, FastMathFlags};
use crate::basic_block::BasicBlock;
use crate::function::Function;
use crate::module::Module;

pub struct Printer<'a> {
    ctx: &'a Context,
}

impl<'a> Printer<'a> {
    pub fn new(ctx: &'a Context) -> Self {
        Printer { ctx }
    }

    pub fn print_module(&self, module: &Module) -> String {
        let mut out = String::new();

        // Module header
        if let Some(ref sf) = module.source_filename {
            writeln!(out, "source_filename = {:?}", sf).unwrap();
        }
        if let Some(ref dl) = module.data_layout {
            writeln!(out, "target datalayout = {:?}", dl).unwrap();
        }
        if let Some(ref tt) = module.target_triple {
            writeln!(out, "target triple = {:?}", tt).unwrap();
        }

        // Named struct type definitions
        if !module.named_types.is_empty() {
            writeln!(out).unwrap();
            for (name, ty) in &module.named_types {
                write!(out, "%{} = type ", name).unwrap();
                if let TypeData::Struct(st) = self.ctx.get_type(*ty) {
                    self.write_struct_body(&mut out, st);
                } else {
                    self.write_type(&mut out, *ty);
                }
                writeln!(out).unwrap();
            }
        }

        // Global variables
        if !module.globals.is_empty() {
            writeln!(out).unwrap();
            for gv in &module.globals {
                let linkage = gv.linkage.as_str();
                if !linkage.is_empty() {
                    write!(out, "@{} = {} ", gv.name, linkage).unwrap();
                } else {
                    write!(out, "@{} = ", gv.name).unwrap();
                }
                if gv.is_constant {
                    write!(out, "constant ").unwrap();
                } else {
                    write!(out, "global ").unwrap();
                }
                self.write_type(&mut out, gv.ty);
                if let Some(init) = gv.initializer {
                    write!(out, " ").unwrap();
                    self.write_const_with_type(&mut out, init);
                } else {
                    write!(out, " undef").unwrap();
                }
                writeln!(out).unwrap();
            }
        }

        // Functions
        for func in &module.functions {
            writeln!(out).unwrap();
            self.write_function(&mut out, func);
        }

        out
    }

    // -----------------------------------------------------------------------
    // Type printing
    // -----------------------------------------------------------------------

    fn write_type(&self, out: &mut String, ty: TypeId) {
        match self.ctx.get_type(ty) {
            TypeData::Void     => out.push_str("void"),
            TypeData::Integer(bits) => { write!(out, "i{}", bits).unwrap(); }
            TypeData::Float(kind)   => out.push_str(float_kind_str(*kind)),
            TypeData::Pointer  => out.push_str("ptr"),
            TypeData::Label    => out.push_str("label"),
            TypeData::Metadata => out.push_str("metadata"),
            TypeData::Array { element, len } => {
                let elem = *element;
                let l = *len;
                write!(out, "[{} x ", l).unwrap();
                self.write_type(out, elem);
                out.push(']');
            }
            TypeData::Vector { element, len, scalable } => {
                let elem = *element;
                let l = *len;
                let sc = *scalable;
                if sc {
                    write!(out, "<vscale x {} x ", l).unwrap();
                } else {
                    write!(out, "<{} x ", l).unwrap();
                }
                self.write_type(out, elem);
                out.push('>');
            }
            TypeData::Struct(st) => {
                if let Some(ref name) = st.name.clone() {
                    write!(out, "%{}", name).unwrap();
                } else {
                    let st = st.clone();
                    self.write_struct_body(out, &st);
                }
            }
            TypeData::Function(ft) => {
                let ft = ft.clone();
                self.write_type(out, ft.ret);
                out.push_str(" (");
                for (i, &p) in ft.params.iter().enumerate() {
                    if i > 0 { out.push_str(", "); }
                    self.write_type(out, p);
                }
                if ft.variadic {
                    if !ft.params.is_empty() { out.push_str(", "); }
                    out.push_str("...");
                }
                out.push(')');
            }
        }
    }

    fn write_struct_body(&self, out: &mut String, st: &StructType) {
        if st.packed { out.push('<'); }
        out.push_str("{ ");
        for (i, &f) in st.fields.iter().enumerate() {
            if i > 0 { out.push_str(", "); }
            self.write_type(out, f);
        }
        out.push_str(" }");
        if st.packed { out.push('>'); }
    }

    // -----------------------------------------------------------------------
    // Constant printing
    // -----------------------------------------------------------------------

    /// Print `type value` e.g. `i32 42`.
    fn write_const_with_type(&self, out: &mut String, id: ConstId) {
        let ty = self.ctx.type_of_const(id);
        self.write_type(out, ty);
        out.push(' ');
        self.write_const_value(out, id);
    }

    fn write_const_value(&self, out: &mut String, id: ConstId) {
        match self.ctx.get_const(id) {
            ConstantData::Int { val, .. } => { write!(out, "{}", *val as i64).unwrap(); }
            ConstantData::IntWide { words, .. } => {
                // Emit as hex for simplicity.
                out.push_str("0x");
                for w in words.iter().rev() {
                    write!(out, "{:016x}", w).unwrap();
                }
            }
            ConstantData::Float { ty, bits } => {
                match self.ctx.get_type(*ty) {
                    TypeData::Float(FloatKind::Single) => {
                        let f = f32::from_bits(*bits as u32);
                        // Use LLVM hex format for exact round-trip.
                        let d = f as f64;
                        write!(out, "0x{:016X}", d.to_bits()).unwrap();
                    }
                    _ => {
                        // Always use hex float for exact round-trip.
                        write!(out, "0x{:016X}", bits).unwrap();
                    }
                }
            }
            ConstantData::Null(_)            => out.push_str("null"),
            ConstantData::Undef(_)           => out.push_str("undef"),
            ConstantData::Poison(_)          => out.push_str("poison"),
            ConstantData::ZeroInitializer(_) => out.push_str("zeroinitializer"),
            ConstantData::Array { elements, .. } => {
                out.push('[');
                for (i, &e) in elements.iter().enumerate() {
                    if i > 0 { out.push_str(", "); }
                    self.write_const_with_type(out, e);
                }
                out.push(']');
            }
            ConstantData::Struct { fields, .. } => {
                out.push_str("{ ");
                for (i, &f) in fields.iter().enumerate() {
                    if i > 0 { out.push_str(", "); }
                    self.write_const_with_type(out, f);
                }
                out.push_str(" }");
            }
            ConstantData::Vector { elements, .. } => {
                out.push('<');
                for (i, &e) in elements.iter().enumerate() {
                    if i > 0 { out.push_str(", "); }
                    self.write_const_with_type(out, e);
                }
                out.push('>');
            }
            ConstantData::GlobalRef { id: gid, .. } => {
                // We don't have the module here; just emit placeholder.
                write!(out, "@g{}", gid.0).unwrap();
            }
        }
    }

    // -----------------------------------------------------------------------
    // ValueRef printing
    // -----------------------------------------------------------------------

    /// Print a typed value ref: `i32 %x` or `i32 42`.
    fn write_typed_value(&self, out: &mut String, vref: ValueRef, func: &Function) {
        let ty = self.type_of_vref(vref, func);
        self.write_type(out, ty);
        out.push(' ');
        self.write_value(out, vref, func);
    }

    fn write_value(&self, out: &mut String, vref: ValueRef, func: &Function) {
        match vref {
            ValueRef::Instruction(id) => {
                if let Some(ref name) = func.instr(id).name {
                    write!(out, "%{}", name).unwrap();
                } else {
                    write!(out, "%v{}", id.0).unwrap();
                }
            }
            ValueRef::Argument(id) => {
                let arg = func.arg(id);
                if arg.name.is_empty() {
                    write!(out, "%{}", id.0).unwrap();
                } else {
                    write!(out, "%{}", arg.name).unwrap();
                }
            }
            ValueRef::Constant(id) => self.write_const_value(out, id),
            ValueRef::Global(id) => { write!(out, "@g{}", id.0).unwrap(); }
        }
    }

    fn type_of_vref(&self, vref: ValueRef, func: &Function) -> TypeId {
        match vref {
            ValueRef::Instruction(id) => func.instr(id).ty,
            ValueRef::Argument(id)    => func.arg(id).ty,
            ValueRef::Constant(id)    => self.ctx.type_of_const(id),
            ValueRef::Global(_)       => self.ctx.ptr_ty,
        }
    }

    // -----------------------------------------------------------------------
    // Function printing
    // -----------------------------------------------------------------------

    fn write_function(&self, out: &mut String, func: &Function) {
        if func.is_declaration {
            out.push_str("declare ");
        } else {
            out.push_str("define ");
        }
        // Linkage (skip for external which is default).
        let lk = func.linkage.as_str();
        if !lk.is_empty() {
            write!(out, "{} ", lk).unwrap();
        }

        // Return type from function type.
        if let crate::types::TypeData::Function(ft) = self.ctx.get_type(func.ty) {
            let ret = ft.ret;
            let params = ft.params.clone();
            let variadic = ft.variadic;
            self.write_type(out, ret);
            write!(out, " @{}(", func.name).unwrap();
            for (i, arg) in func.args.iter().enumerate() {
                if i > 0 { out.push_str(", "); }
                self.write_type(out, arg.ty);
                if !arg.name.is_empty() {
                    write!(out, " %{}", arg.name).unwrap();
                }
            }
            // If there are fewer args than param types (anonymous decl args), fill in.
            let n_printed = func.args.len();
            if n_printed < params.len() {
                for i in n_printed..params.len() {
                    if i > 0 { out.push_str(", "); }
                    self.write_type(out, params[i]);
                }
            }
            if variadic {
                if !func.args.is_empty() || !params.is_empty() { out.push_str(", "); }
                out.push_str("...");
            }
        } else {
            write!(out, "??? @{}(", func.name).unwrap();
        }
        out.push(')');

        if func.is_declaration {
            writeln!(out).unwrap();
            return;
        }

        writeln!(out, " {{").unwrap();

        for (bi, bb) in func.blocks.iter().enumerate() {
            self.write_block(out, bb, func, BlockId(bi as u32));
        }

        writeln!(out, "}}").unwrap();
    }

    fn write_block(&self, out: &mut String, bb: &BasicBlock, func: &Function, _bid: BlockId) {
        writeln!(out, "{}:", bb.name).unwrap();
        for id in bb.instrs() {
            self.write_instr(out, id, func);
        }
    }

    fn write_instr(&self, out: &mut String, id: InstrId, func: &Function) {
        let instr = func.instr(id);
        out.push_str("  ");

        // Result name if non-void.
        if let Some(ref name) = instr.name {
            if !name.is_empty() {
                write!(out, "%{} = ", name).unwrap();
            }
        }

        // Emit fast-math flags helper.
        fn fmf_str(f: &FastMathFlags) -> String {
            let mut s = String::new();
            if f.fast    { s.push_str("fast "); return s; }
            if f.nnan    { s.push_str("nnan "); }
            if f.ninf    { s.push_str("ninf "); }
            if f.nsz     { s.push_str("nsz "); }
            if f.arcp    { s.push_str("arcp "); }
            if f.contract{ s.push_str("contract "); }
            if f.afn     { s.push_str("afn "); }
            if f.reassoc { s.push_str("reassoc "); }
            s
        }

        match &instr.kind {
            InstrKind::Add { flags, lhs, rhs } => {
                out.push_str("add ");
                if flags.nuw { out.push_str("nuw "); }
                if flags.nsw { out.push_str("nsw "); }
                self.write_typed_value(out, *lhs, func);
                out.push_str(", ");
                self.write_value(out, *rhs, func);
            }
            InstrKind::Sub { flags, lhs, rhs } => {
                out.push_str("sub ");
                if flags.nuw { out.push_str("nuw "); }
                if flags.nsw { out.push_str("nsw "); }
                self.write_typed_value(out, *lhs, func);
                out.push_str(", ");
                self.write_value(out, *rhs, func);
            }
            InstrKind::Mul { flags, lhs, rhs } => {
                out.push_str("mul ");
                if flags.nuw { out.push_str("nuw "); }
                if flags.nsw { out.push_str("nsw "); }
                self.write_typed_value(out, *lhs, func);
                out.push_str(", ");
                self.write_value(out, *rhs, func);
            }
            InstrKind::UDiv { exact, lhs, rhs } => {
                out.push_str("udiv ");
                if *exact { out.push_str("exact "); }
                self.write_typed_value(out, *lhs, func);
                out.push_str(", ");
                self.write_value(out, *rhs, func);
            }
            InstrKind::SDiv { exact, lhs, rhs } => {
                out.push_str("sdiv ");
                if *exact { out.push_str("exact "); }
                self.write_typed_value(out, *lhs, func);
                out.push_str(", ");
                self.write_value(out, *rhs, func);
            }
            InstrKind::URem { lhs, rhs } => {
                out.push_str("urem ");
                self.write_typed_value(out, *lhs, func);
                out.push_str(", ");
                self.write_value(out, *rhs, func);
            }
            InstrKind::SRem { lhs, rhs } => {
                out.push_str("srem ");
                self.write_typed_value(out, *lhs, func);
                out.push_str(", ");
                self.write_value(out, *rhs, func);
            }
            InstrKind::And { lhs, rhs } => {
                out.push_str("and ");
                self.write_typed_value(out, *lhs, func);
                out.push_str(", ");
                self.write_value(out, *rhs, func);
            }
            InstrKind::Or { lhs, rhs } => {
                out.push_str("or ");
                self.write_typed_value(out, *lhs, func);
                out.push_str(", ");
                self.write_value(out, *rhs, func);
            }
            InstrKind::Xor { lhs, rhs } => {
                out.push_str("xor ");
                self.write_typed_value(out, *lhs, func);
                out.push_str(", ");
                self.write_value(out, *rhs, func);
            }
            InstrKind::Shl { flags, lhs, rhs } => {
                out.push_str("shl ");
                if flags.nuw { out.push_str("nuw "); }
                if flags.nsw { out.push_str("nsw "); }
                self.write_typed_value(out, *lhs, func);
                out.push_str(", ");
                self.write_value(out, *rhs, func);
            }
            InstrKind::LShr { exact, lhs, rhs } => {
                out.push_str("lshr ");
                if *exact { out.push_str("exact "); }
                self.write_typed_value(out, *lhs, func);
                out.push_str(", ");
                self.write_value(out, *rhs, func);
            }
            InstrKind::AShr { exact, lhs, rhs } => {
                out.push_str("ashr ");
                if *exact { out.push_str("exact "); }
                self.write_typed_value(out, *lhs, func);
                out.push_str(", ");
                self.write_value(out, *rhs, func);
            }
            InstrKind::FAdd { flags, lhs, rhs } => {
                write!(out, "fadd {}", fmf_str(flags)).unwrap();
                self.write_typed_value(out, *lhs, func);
                out.push_str(", ");
                self.write_value(out, *rhs, func);
            }
            InstrKind::FSub { flags, lhs, rhs } => {
                write!(out, "fsub {}", fmf_str(flags)).unwrap();
                self.write_typed_value(out, *lhs, func);
                out.push_str(", ");
                self.write_value(out, *rhs, func);
            }
            InstrKind::FMul { flags, lhs, rhs } => {
                write!(out, "fmul {}", fmf_str(flags)).unwrap();
                self.write_typed_value(out, *lhs, func);
                out.push_str(", ");
                self.write_value(out, *rhs, func);
            }
            InstrKind::FDiv { flags, lhs, rhs } => {
                write!(out, "fdiv {}", fmf_str(flags)).unwrap();
                self.write_typed_value(out, *lhs, func);
                out.push_str(", ");
                self.write_value(out, *rhs, func);
            }
            InstrKind::FRem { flags, lhs, rhs } => {
                write!(out, "frem {}", fmf_str(flags)).unwrap();
                self.write_typed_value(out, *lhs, func);
                out.push_str(", ");
                self.write_value(out, *rhs, func);
            }
            InstrKind::FNeg { flags, operand } => {
                write!(out, "fneg {}", fmf_str(flags)).unwrap();
                self.write_typed_value(out, *operand, func);
            }
            InstrKind::ICmp { pred, lhs, rhs } => {
                write!(out, "icmp {} ", pred.as_str()).unwrap();
                self.write_typed_value(out, *lhs, func);
                out.push_str(", ");
                self.write_value(out, *rhs, func);
            }
            InstrKind::FCmp { flags, pred, lhs, rhs } => {
                write!(out, "fcmp {}{} ", fmf_str(flags), pred.as_str()).unwrap();
                self.write_typed_value(out, *lhs, func);
                out.push_str(", ");
                self.write_value(out, *rhs, func);
            }
            InstrKind::Alloca { alloc_ty, num_elements, align } => {
                out.push_str("alloca ");
                self.write_type(out, *alloc_ty);
                if let Some(ne) = num_elements {
                    out.push_str(", ");
                    self.write_typed_value(out, *ne, func);
                }
                if let Some(a) = align {
                    write!(out, ", align {}", a).unwrap();
                }
            }
            InstrKind::Load { ty, ptr, align, volatile } => {
                if *volatile { out.push_str("volatile "); }
                out.push_str("load ");
                self.write_type(out, *ty);
                out.push_str(", ");
                self.write_typed_value(out, *ptr, func);
                if let Some(a) = align {
                    write!(out, ", align {}", a).unwrap();
                }
            }
            InstrKind::Store { val, ptr, align, volatile } => {
                if *volatile { out.push_str("volatile "); }
                out.push_str("store ");
                self.write_typed_value(out, *val, func);
                out.push_str(", ");
                self.write_typed_value(out, *ptr, func);
                if let Some(a) = align {
                    write!(out, ", align {}", a).unwrap();
                }
            }
            InstrKind::GetElementPtr { inbounds, base_ty, ptr, indices } => {
                out.push_str("getelementptr ");
                if *inbounds { out.push_str("inbounds "); }
                self.write_type(out, *base_ty);
                out.push_str(", ");
                self.write_typed_value(out, *ptr, func);
                for idx in indices {
                    out.push_str(", ");
                    self.write_typed_value(out, *idx, func);
                }
            }
            InstrKind::Trunc { val, to }
            | InstrKind::ZExt { val, to }
            | InstrKind::SExt { val, to }
            | InstrKind::FPTrunc { val, to }
            | InstrKind::FPExt { val, to }
            | InstrKind::FPToUI { val, to }
            | InstrKind::FPToSI { val, to }
            | InstrKind::UIToFP { val, to }
            | InstrKind::SIToFP { val, to }
            | InstrKind::PtrToInt { val, to }
            | InstrKind::IntToPtr { val, to }
            | InstrKind::BitCast { val, to }
            | InstrKind::AddrSpaceCast { val, to } => {
                out.push_str(instr.kind.opcode());
                out.push(' ');
                self.write_typed_value(out, *val, func);
                out.push_str(" to ");
                self.write_type(out, *to);
            }
            InstrKind::Select { cond, then_val, else_val } => {
                out.push_str("select ");
                self.write_typed_value(out, *cond, func);
                out.push_str(", ");
                self.write_typed_value(out, *then_val, func);
                out.push_str(", ");
                self.write_typed_value(out, *else_val, func);
            }
            InstrKind::Phi { ty, incoming } => {
                out.push_str("phi ");
                self.write_type(out, *ty);
                let inc = incoming.clone();
                for (i, (val, block)) in inc.iter().enumerate() {
                    if i > 0 { out.push_str(", "); } else { out.push(' '); }
                    out.push_str("[ ");
                    self.write_value(out, *val, func);
                    out.push_str(", %");
                    out.push_str(&func.block(*block).name);
                    out.push_str(" ]");
                }
            }
            InstrKind::ExtractValue { aggregate, indices } => {
                out.push_str("extractvalue ");
                self.write_typed_value(out, *aggregate, func);
                for &i in indices {
                    write!(out, ", {}", i).unwrap();
                }
            }
            InstrKind::InsertValue { aggregate, val, indices } => {
                out.push_str("insertvalue ");
                self.write_typed_value(out, *aggregate, func);
                out.push_str(", ");
                self.write_typed_value(out, *val, func);
                for &i in indices {
                    write!(out, ", {}", i).unwrap();
                }
            }
            InstrKind::ExtractElement { vec, idx } => {
                out.push_str("extractelement ");
                self.write_typed_value(out, *vec, func);
                out.push_str(", ");
                self.write_typed_value(out, *idx, func);
            }
            InstrKind::InsertElement { vec, val, idx } => {
                out.push_str("insertelement ");
                self.write_typed_value(out, *vec, func);
                out.push_str(", ");
                self.write_typed_value(out, *val, func);
                out.push_str(", ");
                self.write_typed_value(out, *idx, func);
            }
            InstrKind::ShuffleVector { v1, v2, mask } => {
                out.push_str("shufflevector ");
                self.write_typed_value(out, *v1, func);
                out.push_str(", ");
                self.write_typed_value(out, *v2, func);
                out.push_str(", <");
                for (i, &m) in mask.iter().enumerate() {
                    if i > 0 { out.push_str(", "); }
                    write!(out, "i32 {}", m).unwrap();
                }
                out.push('>');
            }
            InstrKind::Call { tail, callee_ty, callee, args } => {
                match tail {
                    crate::instruction::TailCallKind::Tail    => out.push_str("tail "),
                    crate::instruction::TailCallKind::MustTail => out.push_str("musttail "),
                    crate::instruction::TailCallKind::NoTail  => out.push_str("notail "),
                    crate::instruction::TailCallKind::None    => {}
                }
                out.push_str("call ");
                // Print return type from callee_ty.
                if let crate::types::TypeData::Function(ft) = self.ctx.get_type(*callee_ty) {
                    let ret = ft.ret;
                    self.write_type(out, ret);
                    out.push(' ');
                }
                self.write_value(out, *callee, func);
                out.push('(');
                let call_args = args.clone();
                for (i, &arg) in call_args.iter().enumerate() {
                    if i > 0 { out.push_str(", "); }
                    self.write_typed_value(out, arg, func);
                }
                out.push(')');
            }
            InstrKind::Ret { val: None } => {
                out.push_str("ret void");
            }
            InstrKind::Ret { val: Some(v) } => {
                out.push_str("ret ");
                self.write_typed_value(out, *v, func);
            }
            InstrKind::Br { dest } => {
                write!(out, "br label %{}", func.block(*dest).name).unwrap();
            }
            InstrKind::CondBr { cond, then_dest, else_dest } => {
                out.push_str("br ");
                self.write_typed_value(out, *cond, func);
                write!(
                    out,
                    ", label %{}, label %{}",
                    func.block(*then_dest).name,
                    func.block(*else_dest).name
                ).unwrap();
            }
            InstrKind::Switch { val, default, cases } => {
                out.push_str("switch ");
                self.write_typed_value(out, *val, func);
                write!(out, ", label %{} [\n", func.block(*default).name).unwrap();
                let sw_cases = cases.clone();
                for (case_val, dest) in &sw_cases {
                    out.push_str("    ");
                    self.write_typed_value(out, *case_val, func);
                    write!(out, ", label %{}\n", func.block(*dest).name).unwrap();
                }
                out.push_str("  ]");
            }
            InstrKind::Unreachable => {
                out.push_str("unreachable");
            }
        }
        writeln!(out).unwrap();
    }
}

fn float_kind_str(kind: FloatKind) -> &'static str {
    match kind {
        FloatKind::Half   => "half",
        FloatKind::BFloat => "bfloat",
        FloatKind::Single => "float",
        FloatKind::Double => "double",
        FloatKind::Fp128  => "fp128",
        FloatKind::X86Fp80 => "x86_fp80",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::Context;
    use crate::module::Module;
    use crate::function::Function;
    use crate::basic_block::BasicBlock;
    use crate::instruction::{Instruction, InstrKind, IntArithFlags};
    use crate::value::Linkage;

    #[test]
    fn print_simple_function() {
        let mut ctx = Context::new();
        let fn_ty = ctx.mk_fn_type(ctx.i32_ty, vec![ctx.i32_ty, ctx.i32_ty], false);
        let args = vec![
            crate::value::Argument { name: "a".into(), ty: ctx.i32_ty, index: 0 },
            crate::value::Argument { name: "b".into(), ty: ctx.i32_ty, index: 1 },
        ];
        let mut func = Function::new("add", fn_ty, args, Linkage::External);

        let a_ref = ValueRef::Argument(crate::context::ArgId(0));
        let b_ref = ValueRef::Argument(crate::context::ArgId(1));

        let mut bb = BasicBlock::new("entry");
        let add_instr = Instruction::new(
            Some("result".into()),
            ctx.i32_ty,
            InstrKind::Add { flags: IntArithFlags::default(), lhs: a_ref, rhs: b_ref },
        );
        let iid = func.alloc_instr(add_instr);
        bb.append_instr(iid);

        let ret_instr = Instruction::new(
            None,
            ctx.void_ty,
            InstrKind::Ret { val: Some(ValueRef::Instruction(iid)) },
        );
        let rid = func.alloc_instr(ret_instr);
        bb.set_terminator(rid);

        let _bid = func.add_block(bb);

        let mut module = Module::new("test");
        module.add_function(func);

        let printer = Printer::new(&ctx);
        let output = printer.print_module(&module);
        assert!(output.contains("define i32 @add("));
        assert!(output.contains("%result = add i32 %a, %b"));
        assert!(output.contains("ret i32 %result"));
    }
}
