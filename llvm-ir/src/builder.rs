//! IR builder API for programmatic construction of instructions and basic blocks.

use crate::context::{Context, TypeId, FunctionId, BlockId, ConstId, GlobalId, ValueRef};
use crate::value::{Argument, Linkage, GlobalVariable};
use crate::function::Function;
use crate::module::Module;
use crate::basic_block::BasicBlock;
use crate::instruction::{Instruction, InstrKind, IntArithFlags, FastMathFlags,
                         IntPredicate, FloatPredicate, TailCallKind};

/// Programmatic IR builder.
///
/// Holds mutable references to the `Context` and `Module` so that a single
/// builder instance can construct multiple functions and look up types.
pub struct Builder<'a> {
    pub ctx: &'a mut Context,
    pub module: &'a mut Module,
    current_function: Option<FunctionId>,
    current_block: Option<BlockId>,
}

impl<'a> Builder<'a> {
    pub fn new(ctx: &'a mut Context, module: &'a mut Module) -> Self {
        Builder { ctx, module, current_function: None, current_block: None }
    }

    // -----------------------------------------------------------------------
    // Function creation
    // -----------------------------------------------------------------------

    pub fn add_function(
        &mut self,
        name: impl Into<String>,
        ret_ty: TypeId,
        param_tys: Vec<TypeId>,
        param_names: Vec<String>,
        variadic: bool,
        linkage: Linkage,
    ) -> FunctionId {
        let fn_ty = self.ctx.mk_fn_type(ret_ty, param_tys.clone(), variadic);
        let args: Vec<Argument> = param_tys
            .iter()
            .zip(param_names.iter().chain(std::iter::repeat(&String::new())))
            .enumerate()
            .map(|(i, (&ty, name))| Argument { name: name.clone(), ty, index: i as u32 })
            .collect();
        let func = Function::new(name, fn_ty, args, linkage);
        let id = self.module.add_function(func);
        self.current_function = Some(id);
        self.current_block = None;
        id
    }

    pub fn add_declaration(
        &mut self,
        name: impl Into<String>,
        ret_ty: TypeId,
        param_tys: Vec<TypeId>,
        variadic: bool,
    ) -> FunctionId {
        let fn_ty = self.ctx.mk_fn_type(ret_ty, param_tys.clone(), variadic);
        let args: Vec<Argument> = param_tys
            .iter()
            .enumerate()
            .map(|(i, &ty)| Argument { name: String::new(), ty, index: i as u32 })
            .collect();
        let func = Function::new_declaration(name, fn_ty, args, Linkage::External);
        let id = self.module.add_function(func);
        id
    }

    // -----------------------------------------------------------------------
    // Block management
    // -----------------------------------------------------------------------

    pub fn add_block(&mut self, name: impl Into<String>) -> BlockId {
        let fid = self.current_function.expect("no current function");
        let bb = BasicBlock::new(name);
        let bid = self.module.function_mut(fid).add_block(bb);
        bid
    }

    pub fn position_at_end(&mut self, block: BlockId) {
        self.current_block = Some(block);
    }

    pub fn current_function(&self) -> Option<FunctionId> {
        self.current_function
    }

    pub fn current_block(&self) -> Option<BlockId> {
        self.current_block
    }

    // -----------------------------------------------------------------------
    // Argument access
    // -----------------------------------------------------------------------

    pub fn get_arg(&self, index: u32) -> ValueRef {
        ValueRef::Argument(crate::context::ArgId(index))
    }

    // -----------------------------------------------------------------------
    // Constant helpers
    // -----------------------------------------------------------------------

    pub fn const_int(&mut self, ty: TypeId, val: u64) -> ValueRef {
        ValueRef::Constant(self.ctx.const_int(ty, val))
    }

    pub fn const_bool(&mut self, val: bool) -> ValueRef {
        ValueRef::Constant(self.ctx.const_int(self.ctx.i1_ty, val as u64))
    }

    pub fn const_i32(&mut self, val: i32) -> ValueRef {
        let ty = self.ctx.i32_ty;
        ValueRef::Constant(self.ctx.const_int(ty, val as u64))
    }

    pub fn const_i64(&mut self, val: i64) -> ValueRef {
        let ty = self.ctx.i64_ty;
        ValueRef::Constant(self.ctx.const_int(ty, val as u64))
    }

    pub fn const_f32(&mut self, val: f32) -> ValueRef {
        let ty = self.ctx.f32_ty;
        ValueRef::Constant(self.ctx.const_float(ty, val.to_bits() as u64))
    }

    pub fn const_f64(&mut self, val: f64) -> ValueRef {
        let ty = self.ctx.f64_ty;
        ValueRef::Constant(self.ctx.const_float(ty, val.to_bits()))
    }

    pub fn const_null(&mut self, ty: TypeId) -> ValueRef {
        ValueRef::Constant(self.ctx.const_null(ty))
    }

    pub fn undef(&mut self, ty: TypeId) -> ValueRef {
        ValueRef::Constant(self.ctx.const_undef(ty))
    }

    pub fn poison(&mut self, ty: TypeId) -> ValueRef {
        ValueRef::Constant(self.ctx.const_poison(ty))
    }

    pub fn const_zero(&mut self, ty: TypeId) -> ValueRef {
        ValueRef::Constant(self.ctx.const_zero(ty))
    }

    // -----------------------------------------------------------------------
    // Global variables
    // -----------------------------------------------------------------------

    pub fn add_global(
        &mut self,
        name: impl Into<String>,
        ty: TypeId,
        initializer: Option<ConstId>,
        is_constant: bool,
        linkage: Linkage,
    ) -> GlobalId {
        let gv = GlobalVariable { name: name.into(), ty, initializer, is_constant, linkage };
        self.module.add_global(gv)
    }

    // -----------------------------------------------------------------------
    // Private append helper
    // -----------------------------------------------------------------------

    fn append_instr(&mut self, name: Option<String>, ty: TypeId, kind: InstrKind) -> ValueRef {
        let fid = self.current_function.expect("no current function");
        let bid = self.current_block.expect("no current block");
        let instr = Instruction::new(name, ty, kind);
        let is_terminator = instr.is_terminator();
        let id = self.module.function_mut(fid).alloc_instr(instr);
        if is_terminator {
            self.module.function_mut(fid).block_mut(bid).set_terminator(id);
        } else {
            self.module.function_mut(fid).block_mut(bid).append_instr(id);
        }
        ValueRef::Instruction(id)
    }

    // -----------------------------------------------------------------------
    // Integer arithmetic
    // -----------------------------------------------------------------------

    pub fn build_add(&mut self, name: impl Into<String>, lhs: ValueRef, rhs: ValueRef) -> ValueRef {
        let ty = self.type_of(lhs);
        self.append_instr(Some(name.into()), ty,
            InstrKind::Add { flags: IntArithFlags::default(), lhs, rhs })
    }

    pub fn build_add_nsw(&mut self, name: impl Into<String>, lhs: ValueRef, rhs: ValueRef) -> ValueRef {
        let ty = self.type_of(lhs);
        self.append_instr(Some(name.into()), ty,
            InstrKind::Add { flags: IntArithFlags { nsw: true, nuw: false }, lhs, rhs })
    }

    pub fn build_sub(&mut self, name: impl Into<String>, lhs: ValueRef, rhs: ValueRef) -> ValueRef {
        let ty = self.type_of(lhs);
        self.append_instr(Some(name.into()), ty,
            InstrKind::Sub { flags: IntArithFlags::default(), lhs, rhs })
    }

    pub fn build_mul(&mut self, name: impl Into<String>, lhs: ValueRef, rhs: ValueRef) -> ValueRef {
        let ty = self.type_of(lhs);
        self.append_instr(Some(name.into()), ty,
            InstrKind::Mul { flags: IntArithFlags::default(), lhs, rhs })
    }

    pub fn build_udiv(&mut self, name: impl Into<String>, lhs: ValueRef, rhs: ValueRef) -> ValueRef {
        let ty = self.type_of(lhs);
        self.append_instr(Some(name.into()), ty, InstrKind::UDiv { exact: false, lhs, rhs })
    }

    pub fn build_sdiv(&mut self, name: impl Into<String>, lhs: ValueRef, rhs: ValueRef) -> ValueRef {
        let ty = self.type_of(lhs);
        self.append_instr(Some(name.into()), ty, InstrKind::SDiv { exact: false, lhs, rhs })
    }

    pub fn build_urem(&mut self, name: impl Into<String>, lhs: ValueRef, rhs: ValueRef) -> ValueRef {
        let ty = self.type_of(lhs);
        self.append_instr(Some(name.into()), ty, InstrKind::URem { lhs, rhs })
    }

    pub fn build_srem(&mut self, name: impl Into<String>, lhs: ValueRef, rhs: ValueRef) -> ValueRef {
        let ty = self.type_of(lhs);
        self.append_instr(Some(name.into()), ty, InstrKind::SRem { lhs, rhs })
    }

    // --- Bitwise ---

    pub fn build_and(&mut self, name: impl Into<String>, lhs: ValueRef, rhs: ValueRef) -> ValueRef {
        let ty = self.type_of(lhs);
        self.append_instr(Some(name.into()), ty, InstrKind::And { lhs, rhs })
    }

    pub fn build_or(&mut self, name: impl Into<String>, lhs: ValueRef, rhs: ValueRef) -> ValueRef {
        let ty = self.type_of(lhs);
        self.append_instr(Some(name.into()), ty, InstrKind::Or { lhs, rhs })
    }

    pub fn build_xor(&mut self, name: impl Into<String>, lhs: ValueRef, rhs: ValueRef) -> ValueRef {
        let ty = self.type_of(lhs);
        self.append_instr(Some(name.into()), ty, InstrKind::Xor { lhs, rhs })
    }

    pub fn build_shl(&mut self, name: impl Into<String>, lhs: ValueRef, rhs: ValueRef) -> ValueRef {
        let ty = self.type_of(lhs);
        self.append_instr(Some(name.into()), ty,
            InstrKind::Shl { flags: IntArithFlags::default(), lhs, rhs })
    }

    pub fn build_lshr(&mut self, name: impl Into<String>, lhs: ValueRef, rhs: ValueRef) -> ValueRef {
        let ty = self.type_of(lhs);
        self.append_instr(Some(name.into()), ty, InstrKind::LShr { exact: false, lhs, rhs })
    }

    pub fn build_ashr(&mut self, name: impl Into<String>, lhs: ValueRef, rhs: ValueRef) -> ValueRef {
        let ty = self.type_of(lhs);
        self.append_instr(Some(name.into()), ty, InstrKind::AShr { exact: false, lhs, rhs })
    }

    // --- FP arithmetic ---

    pub fn build_fadd(&mut self, name: impl Into<String>, lhs: ValueRef, rhs: ValueRef) -> ValueRef {
        let ty = self.type_of(lhs);
        self.append_instr(Some(name.into()), ty,
            InstrKind::FAdd { flags: FastMathFlags::default(), lhs, rhs })
    }

    pub fn build_fsub(&mut self, name: impl Into<String>, lhs: ValueRef, rhs: ValueRef) -> ValueRef {
        let ty = self.type_of(lhs);
        self.append_instr(Some(name.into()), ty,
            InstrKind::FSub { flags: FastMathFlags::default(), lhs, rhs })
    }

    pub fn build_fmul(&mut self, name: impl Into<String>, lhs: ValueRef, rhs: ValueRef) -> ValueRef {
        let ty = self.type_of(lhs);
        self.append_instr(Some(name.into()), ty,
            InstrKind::FMul { flags: FastMathFlags::default(), lhs, rhs })
    }

    pub fn build_fdiv(&mut self, name: impl Into<String>, lhs: ValueRef, rhs: ValueRef) -> ValueRef {
        let ty = self.type_of(lhs);
        self.append_instr(Some(name.into()), ty,
            InstrKind::FDiv { flags: FastMathFlags::default(), lhs, rhs })
    }

    pub fn build_fneg(&mut self, name: impl Into<String>, val: ValueRef) -> ValueRef {
        let ty = self.type_of(val);
        self.append_instr(Some(name.into()), ty,
            InstrKind::FNeg { flags: FastMathFlags::default(), operand: val })
    }

    // --- Comparisons ---

    pub fn build_icmp(&mut self, name: impl Into<String>, pred: IntPredicate, lhs: ValueRef, rhs: ValueRef) -> ValueRef {
        let i1 = self.ctx.i1_ty;
        self.append_instr(Some(name.into()), i1, InstrKind::ICmp { pred, lhs, rhs })
    }

    pub fn build_fcmp(&mut self, name: impl Into<String>, pred: FloatPredicate, lhs: ValueRef, rhs: ValueRef) -> ValueRef {
        let i1 = self.ctx.i1_ty;
        self.append_instr(Some(name.into()), i1,
            InstrKind::FCmp { flags: FastMathFlags::default(), pred, lhs, rhs })
    }

    // --- Memory ---

    pub fn build_alloca(&mut self, name: impl Into<String>, alloc_ty: TypeId) -> ValueRef {
        let ptr_ty = self.ctx.ptr_ty;
        self.append_instr(Some(name.into()), ptr_ty,
            InstrKind::Alloca { alloc_ty, num_elements: None, align: None })
    }

    pub fn build_alloca_aligned(&mut self, name: impl Into<String>, alloc_ty: TypeId, align: u32) -> ValueRef {
        let ptr_ty = self.ctx.ptr_ty;
        self.append_instr(Some(name.into()), ptr_ty,
            InstrKind::Alloca { alloc_ty, num_elements: None, align: Some(align) })
    }

    pub fn build_load(&mut self, name: impl Into<String>, ty: TypeId, ptr: ValueRef) -> ValueRef {
        self.append_instr(Some(name.into()), ty,
            InstrKind::Load { ty, ptr, align: None, volatile: false })
    }

    pub fn build_load_aligned(&mut self, name: impl Into<String>, ty: TypeId, ptr: ValueRef, align: u32) -> ValueRef {
        self.append_instr(Some(name.into()), ty,
            InstrKind::Load { ty, ptr, align: Some(align), volatile: false })
    }

    pub fn build_store(&mut self, val: ValueRef, ptr: ValueRef) -> ValueRef {
        let void_ty = self.ctx.void_ty;
        self.append_instr(None, void_ty,
            InstrKind::Store { val, ptr, align: None, volatile: false })
    }

    pub fn build_store_aligned(&mut self, val: ValueRef, ptr: ValueRef, align: u32) -> ValueRef {
        let void_ty = self.ctx.void_ty;
        self.append_instr(None, void_ty,
            InstrKind::Store { val, ptr, align: Some(align), volatile: false })
    }

    pub fn build_gep(&mut self, name: impl Into<String>, base_ty: TypeId, ptr: ValueRef, indices: Vec<ValueRef>) -> ValueRef {
        let ptr_ty = self.ctx.ptr_ty;
        self.append_instr(Some(name.into()), ptr_ty,
            InstrKind::GetElementPtr { inbounds: false, base_ty, ptr, indices })
    }

    pub fn build_gep_inbounds(&mut self, name: impl Into<String>, base_ty: TypeId, ptr: ValueRef, indices: Vec<ValueRef>) -> ValueRef {
        let ptr_ty = self.ctx.ptr_ty;
        self.append_instr(Some(name.into()), ptr_ty,
            InstrKind::GetElementPtr { inbounds: true, base_ty, ptr, indices })
    }

    // --- Casts ---

    pub fn build_trunc(&mut self, name: impl Into<String>, val: ValueRef, to: TypeId) -> ValueRef {
        self.append_instr(Some(name.into()), to, InstrKind::Trunc { val, to })
    }
    pub fn build_zext(&mut self, name: impl Into<String>, val: ValueRef, to: TypeId) -> ValueRef {
        self.append_instr(Some(name.into()), to, InstrKind::ZExt { val, to })
    }
    pub fn build_sext(&mut self, name: impl Into<String>, val: ValueRef, to: TypeId) -> ValueRef {
        self.append_instr(Some(name.into()), to, InstrKind::SExt { val, to })
    }
    pub fn build_fptrunc(&mut self, name: impl Into<String>, val: ValueRef, to: TypeId) -> ValueRef {
        self.append_instr(Some(name.into()), to, InstrKind::FPTrunc { val, to })
    }
    pub fn build_fpext(&mut self, name: impl Into<String>, val: ValueRef, to: TypeId) -> ValueRef {
        self.append_instr(Some(name.into()), to, InstrKind::FPExt { val, to })
    }
    pub fn build_fptoui(&mut self, name: impl Into<String>, val: ValueRef, to: TypeId) -> ValueRef {
        self.append_instr(Some(name.into()), to, InstrKind::FPToUI { val, to })
    }
    pub fn build_fptosi(&mut self, name: impl Into<String>, val: ValueRef, to: TypeId) -> ValueRef {
        self.append_instr(Some(name.into()), to, InstrKind::FPToSI { val, to })
    }
    pub fn build_uitofp(&mut self, name: impl Into<String>, val: ValueRef, to: TypeId) -> ValueRef {
        self.append_instr(Some(name.into()), to, InstrKind::UIToFP { val, to })
    }
    pub fn build_sitofp(&mut self, name: impl Into<String>, val: ValueRef, to: TypeId) -> ValueRef {
        self.append_instr(Some(name.into()), to, InstrKind::SIToFP { val, to })
    }
    pub fn build_ptrtoint(&mut self, name: impl Into<String>, val: ValueRef, to: TypeId) -> ValueRef {
        self.append_instr(Some(name.into()), to, InstrKind::PtrToInt { val, to })
    }
    pub fn build_inttoptr(&mut self, name: impl Into<String>, val: ValueRef, to: TypeId) -> ValueRef {
        self.append_instr(Some(name.into()), to, InstrKind::IntToPtr { val, to })
    }
    pub fn build_bitcast(&mut self, name: impl Into<String>, val: ValueRef, to: TypeId) -> ValueRef {
        self.append_instr(Some(name.into()), to, InstrKind::BitCast { val, to })
    }

    // --- Misc ---

    pub fn build_select(&mut self, name: impl Into<String>, cond: ValueRef, then_val: ValueRef, else_val: ValueRef) -> ValueRef {
        let ty = self.type_of(then_val);
        self.append_instr(Some(name.into()), ty, InstrKind::Select { cond, then_val, else_val })
    }

    pub fn build_phi(&mut self, name: impl Into<String>, ty: TypeId, incoming: Vec<(ValueRef, BlockId)>) -> ValueRef {
        self.append_instr(Some(name.into()), ty, InstrKind::Phi { ty, incoming })
    }

    pub fn build_extractvalue(&mut self, name: impl Into<String>, aggregate: ValueRef, result_ty: TypeId, indices: Vec<u32>) -> ValueRef {
        self.append_instr(Some(name.into()), result_ty, InstrKind::ExtractValue { aggregate, indices })
    }

    pub fn build_insertvalue(&mut self, name: impl Into<String>, aggregate: ValueRef, val: ValueRef, indices: Vec<u32>) -> ValueRef {
        let ty = self.type_of(aggregate);
        self.append_instr(Some(name.into()), ty, InstrKind::InsertValue { aggregate, val, indices })
    }

    // --- Call ---

    pub fn build_call(
        &mut self,
        name: impl Into<String>,
        ret_ty: TypeId,
        callee_ty: TypeId,
        callee: ValueRef,
        args: Vec<ValueRef>,
    ) -> ValueRef {
        let n = name.into();
        let result_name = if ret_ty == self.ctx.void_ty { None } else { Some(n) };
        self.append_instr(result_name, ret_ty, InstrKind::Call {
            tail: TailCallKind::None,
            callee_ty,
            callee,
            args,
        })
    }

    // --- Terminators ---

    pub fn build_ret_void(&mut self) -> ValueRef {
        let void_ty = self.ctx.void_ty;
        self.append_instr(None, void_ty, InstrKind::Ret { val: None })
    }

    pub fn build_ret(&mut self, val: ValueRef) -> ValueRef {
        let void_ty = self.ctx.void_ty;
        self.append_instr(None, void_ty, InstrKind::Ret { val: Some(val) })
    }

    pub fn build_br(&mut self, dest: BlockId) -> ValueRef {
        let void_ty = self.ctx.void_ty;
        self.append_instr(None, void_ty, InstrKind::Br { dest })
    }

    pub fn build_cond_br(&mut self, cond: ValueRef, then_dest: BlockId, else_dest: BlockId) -> ValueRef {
        let void_ty = self.ctx.void_ty;
        self.append_instr(None, void_ty, InstrKind::CondBr { cond, then_dest, else_dest })
    }

    pub fn build_switch(&mut self, val: ValueRef, default: BlockId, cases: Vec<(ValueRef, BlockId)>) -> ValueRef {
        let void_ty = self.ctx.void_ty;
        self.append_instr(None, void_ty, InstrKind::Switch { val, default, cases })
    }

    pub fn build_unreachable(&mut self) -> ValueRef {
        let void_ty = self.ctx.void_ty;
        self.append_instr(None, void_ty, InstrKind::Unreachable)
    }

    // -----------------------------------------------------------------------
    // Type-of helper (for non-Constant/Global value refs)
    // -----------------------------------------------------------------------

    fn type_of(&self, vref: ValueRef) -> TypeId {
        let fid = self.current_function.expect("no current function");
        let func = self.module.function(fid);
        match vref {
            ValueRef::Instruction(id) => func.instr(id).ty,
            ValueRef::Argument(id)    => func.arg(id).ty,
            ValueRef::Constant(id)    => self.ctx.type_of_const(id),
            ValueRef::Global(_)       => self.ctx.ptr_ty,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::Context;
    use crate::module::Module;
    use crate::printer::Printer;

    #[test]
    fn builder_add_function_and_blocks() {
        let mut ctx = Context::new();
        let mut module = Module::new("test");
        let mut b = Builder::new(&mut ctx, &mut module);

        let _fid = b.add_function(
            "foo",
            b.ctx.i32_ty,
            vec![b.ctx.i32_ty],
            vec!["x".to_string()],
            false,
            Linkage::External,
        );
        let entry = b.add_block("entry");
        b.position_at_end(entry);

        let x = b.get_arg(0);
        let two = b.const_i32(2);
        let result = b.build_mul("result", x, two);
        b.build_ret(result);

        let printer = Printer::new(b.ctx);
        let out = printer.print_module(b.module);
        assert!(out.contains("define i32 @foo("));
        assert!(out.contains("%result = mul i32 %x"));
        assert!(out.contains("ret i32 %result"));
    }

    #[test]
    fn builder_cond_br() {
        let mut ctx = Context::new();
        let mut module = Module::new("test");
        let mut b = Builder::new(&mut ctx, &mut module);

        let _fid = b.add_function(
            "check",
            b.ctx.void_ty,
            vec![b.ctx.i1_ty],
            vec!["cond".to_string()],
            false,
            Linkage::External,
        );
        let entry = b.add_block("entry");
        let then_bb = b.add_block("then");
        let else_bb = b.add_block("else");

        b.position_at_end(entry);
        let cond = b.get_arg(0);
        b.build_cond_br(cond, then_bb, else_bb);

        b.position_at_end(then_bb);
        b.build_ret_void();

        b.position_at_end(else_bb);
        b.build_ret_void();

        let printer = Printer::new(b.ctx);
        let out = printer.print_module(b.module);
        assert!(out.contains("br i1 %cond, label %then, label %else"));
    }
}
