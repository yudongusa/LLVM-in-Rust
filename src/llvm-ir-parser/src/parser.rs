//! Recursive-descent LLVM IR text format parser.
//!
//! Entry point: `parse(src) -> Result<(Context, Module), ParseError>`

use std::collections::HashMap;
use std::fmt;

use llvm_ir::{
    ArgId, Argument, BasicBlock, BlockId, ConstId, ConstantData, Context, FastMathFlags, FloatKind,
    FloatPredicate, Function, GlobalId, GlobalVariable, InstrKind, Instruction, IntArithFlags,
    IntPredicate, Linkage, Module, TailCallKind, TypeId, ValueRef,
};

use crate::lexer::{Keyword, LexError, Lexer, Token};

// ---------------------------------------------------------------------------
// ParseError
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct ParseError {
    pub line: usize,
    pub col: usize,
    pub message: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "parse error at {}:{}: {}",
            self.line, self.col, self.message
        )
    }
}

impl From<LexError> for ParseError {
    fn from(e: LexError) -> Self {
        ParseError {
            line: e.line,
            col: e.col,
            message: e.message,
        }
    }
}

impl From<&LexError> for ParseError {
    fn from(e: &LexError) -> Self {
        ParseError {
            line: e.line,
            col: e.col,
            message: e.message.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// Parser state
// ---------------------------------------------------------------------------

struct Parser<'src> {
    lex: Lexer<'src>,
    ctx: Context,
    module: Module,
    /// Named block forward references: name → BlockId already allocated.
    pending_blocks: HashMap<String, BlockId>,
    /// Current function being parsed (None at module level).
    current_func: Option<usize>, // index into module.functions
    /// Local value table: name → ValueRef, for the current function.
    locals: HashMap<String, ValueRef>,
    /// Unnamed slots: slot number → ValueRef.
    unnamed: HashMap<u64, ValueRef>,
}

impl<'src> Parser<'src> {
    fn new(src: &'src str) -> Self {
        Parser {
            lex: Lexer::new(src),
            ctx: Context::new(),
            module: Module::new(""),
            pending_blocks: HashMap::new(),
            current_func: None,
            locals: HashMap::new(),
            unnamed: HashMap::new(),
        }
    }

    fn err(&self, msg: impl Into<String>) -> ParseError {
        ParseError {
            line: self.lex.current_line(),
            col: self.lex.current_col(),
            message: msg.into(),
        }
    }

    // -----------------------------------------------------------------------
    // Top-level module parsing
    // -----------------------------------------------------------------------

    fn parse_module(&mut self) -> Result<(), ParseError> {
        loop {
            match self.lex.peek()? {
                Token::Eof => break,
                Token::Kw(Keyword::Source) => {
                    self.parse_source_filename()?;
                }
                Token::Kw(Keyword::Target) => {
                    self.parse_target()?;
                }
                Token::LocalIdent(_) => {
                    self.parse_named_type_def()?;
                }
                Token::GlobalIdent(_) => {
                    self.parse_global_or_function()?;
                }
                Token::Kw(Keyword::Define) => {
                    self.parse_function(false)?;
                }
                Token::Kw(Keyword::Declare) => {
                    self.parse_function(true)?;
                }
                Token::Bang => {
                    self.skip_metadata_line()?;
                }
                _ => {
                    let t = self.lex.next()?;
                    return Err(self.err(format!("unexpected top-level token {:?}", t)));
                }
            }
        }
        Ok(())
    }

    fn parse_source_filename(&mut self) -> Result<(), ParseError> {
        self.lex.expect_kw(&Keyword::Source)?;
        self.lex.expect(&Token::Equal)?;
        let s = self.lex.expect_string_lit()?;
        self.module.source_filename = Some(s);
        Ok(())
    }

    fn parse_target(&mut self) -> Result<(), ParseError> {
        self.lex.expect_kw(&Keyword::Target)?;
        match self.lex.next()? {
            Token::Kw(Keyword::Triple) => {
                self.lex.expect(&Token::Equal)?;
                let s = self.lex.expect_string_lit()?;
                self.module.target_triple = Some(s);
            }
            Token::Kw(Keyword::Datalayout) => {
                self.lex.expect(&Token::Equal)?;
                let s = self.lex.expect_string_lit()?;
                self.module.data_layout = Some(s);
            }
            t => return Err(self.err(format!("unexpected after 'target': {:?}", t))),
        }
        Ok(())
    }

    fn parse_named_type_def(&mut self) -> Result<(), ParseError> {
        // %Name = type <body>
        let name = self.lex.expect_local_ident()?;
        self.lex.expect(&Token::Equal)?;
        self.lex.expect_kw(&Keyword::Type)?;
        // Allocate the TypeId now (possibly opaque).
        let ty_id = self.ctx.mk_struct_named(name.clone());
        // Parse body.
        match self.lex.peek()? {
            Token::Kw(Keyword::Void) => {
                // Opaque struct — leave body empty.
                self.lex.next()?;
            }
            _ => {
                let fields = self.parse_struct_body()?;
                self.ctx.define_struct_body(ty_id, fields.0, fields.1);
            }
        }
        self.module.register_named_type(name, ty_id);
        Ok(())
    }

    fn parse_global_or_function(&mut self) -> Result<(), ParseError> {
        // @name = [linkage] (global|constant) type [initializer]
        let name = self.lex.expect_global_ident()?;
        self.lex.expect(&Token::Equal)?;
        let linkage = self.parse_optional_linkage();
        match self.lex.peek()? {
            Token::Kw(Keyword::Global) | Token::Kw(Keyword::Constant) => {
                let is_const = matches!(self.lex.peek()?, Token::Kw(Keyword::Constant));
                self.lex.next()?;
                let ty = self.parse_type()?;
                let init = if !self.at_statement_end() {
                    let c = self.parse_constant(ty)?;
                    Some(c)
                } else {
                    None
                };
                let gv = GlobalVariable {
                    name,
                    ty,
                    initializer: init,
                    is_constant: is_const,
                    linkage,
                };
                self.module.add_global(gv);
            }
            _ => {
                return Err(self.err(format!("expected 'global' or 'constant' for @{}", name)));
            }
        }
        Ok(())
    }

    fn at_statement_end(&mut self) -> bool {
        matches!(
            self.lex.peek(),
            Ok(Token::Eof)
                | Ok(Token::Kw(Keyword::Define))
                | Ok(Token::Kw(Keyword::Declare))
                | Ok(Token::GlobalIdent(_))
                | Ok(Token::LocalIdent(_))
                | Ok(Token::Kw(Keyword::Target))
                | Ok(Token::Kw(Keyword::Source))
                | Ok(Token::Bang)
        )
    }

    // -----------------------------------------------------------------------
    // Linkage
    // -----------------------------------------------------------------------

    fn parse_optional_linkage(&mut self) -> Linkage {
        match self.lex.peek() {
            Ok(Token::Kw(Keyword::Private)) => {
                let _ = self.lex.next();
                Linkage::Private
            }
            Ok(Token::Kw(Keyword::Internal)) => {
                let _ = self.lex.next();
                Linkage::Internal
            }
            Ok(Token::Kw(Keyword::External)) => {
                let _ = self.lex.next();
                Linkage::External
            }
            Ok(Token::Kw(Keyword::Weak)) => {
                let _ = self.lex.next();
                Linkage::Weak
            }
            Ok(Token::Kw(Keyword::WeakOdr)) => {
                let _ = self.lex.next();
                Linkage::WeakOdr
            }
            Ok(Token::Kw(Keyword::Linkonce)) => {
                let _ = self.lex.next();
                Linkage::LinkOnce
            }
            Ok(Token::Kw(Keyword::LinkonceOdr)) => {
                let _ = self.lex.next();
                Linkage::LinkOnceOdr
            }
            Ok(Token::Kw(Keyword::Common)) => {
                let _ = self.lex.next();
                Linkage::Common
            }
            Ok(Token::Kw(Keyword::AvailableExternally)) => {
                let _ = self.lex.next();
                Linkage::AvailableExternally
            }
            _ => Linkage::External,
        }
    }

    // -----------------------------------------------------------------------
    // Type parsing
    // -----------------------------------------------------------------------

    fn parse_type(&mut self) -> Result<TypeId, ParseError> {
        let base = match self.lex.peek()? {
            Token::Kw(Keyword::Void) => {
                self.lex.next()?;
                self.ctx.void_ty
            }
            Token::Kw(Keyword::Half) => {
                self.lex.next()?;
                self.ctx.mk_float(FloatKind::Half)
            }
            Token::Kw(Keyword::Bfloat) => {
                self.lex.next()?;
                self.ctx.mk_float(FloatKind::BFloat)
            }
            Token::Kw(Keyword::Float) => {
                self.lex.next()?;
                self.ctx.f32_ty
            }
            Token::Kw(Keyword::Double) => {
                self.lex.next()?;
                self.ctx.f64_ty
            }
            Token::Kw(Keyword::Fp128) => {
                self.lex.next()?;
                self.ctx.mk_float(FloatKind::Fp128)
            }
            Token::Kw(Keyword::X86Fp80) => {
                self.lex.next()?;
                self.ctx.mk_float(FloatKind::X86Fp80)
            }
            Token::Kw(Keyword::Label) => {
                self.lex.next()?;
                self.ctx.label_ty
            }
            Token::Kw(Keyword::Ptr) => {
                self.lex.next()?;
                self.ctx.ptr_ty
            }
            Token::IntType(bits) => {
                let b = *bits;
                self.lex.next()?;
                self.ctx.mk_int(b)
            }
            Token::LBracket => self.parse_array_type()?,
            Token::LAngle => self.parse_vector_type()?,
            Token::LBrace => {
                let (fields, packed) = self.parse_struct_body()?;
                self.ctx.mk_struct_anon(fields, packed)
            }
            Token::LocalIdent(_) => {
                // Named struct reference: %Name
                let name = self.lex.expect_local_ident()?;
                self.ctx.mk_struct_named(name)
            }
            _ => {
                let t = self.lex.next()?;
                return Err(self.err(format!("expected type, got {:?}", t)));
            }
        };

        // Handle pointer suffix `*` (old-style IR) — consume but return ptr.
        if self.lex.eat(&Token::Star) {
            return Ok(self.ctx.ptr_ty);
        }

        Ok(base)
    }

    fn parse_array_type(&mut self) -> Result<TypeId, ParseError> {
        self.lex.expect(&Token::LBracket)?;
        let len = self.lex.expect_uint_lit()?;
        self.lex.expect_kw(&Keyword::X)?;
        let elem = self.parse_type()?;
        self.lex.expect(&Token::RBracket)?;
        Ok(self.ctx.mk_array(elem, len))
    }

    fn parse_vector_type(&mut self) -> Result<TypeId, ParseError> {
        self.lex.expect(&Token::LAngle)?;
        // Could be `<vscale x N x T>` or `<N x T>`.
        let scalable = self.lex.eat_kw(Keyword::Vscale);
        if scalable {
            self.lex.expect_kw(&Keyword::X)?;
        }
        let len = self.lex.expect_uint_lit()? as u32;
        self.lex.expect_kw(&Keyword::X)?;
        let elem = self.parse_type()?;
        self.lex.expect(&Token::RAngle)?;
        Ok(self.ctx.mk_vector(elem, len, scalable))
    }

    /// Parse `{ field, field, ... }` or `<{ ... }>` (packed).
    fn parse_struct_body(&mut self) -> Result<(Vec<TypeId>, bool), ParseError> {
        let packed = self.lex.eat(&Token::LAngle);
        self.lex.expect(&Token::LBrace)?;
        let mut fields = Vec::new();
        if !matches!(self.lex.peek()?, Token::RBrace) {
            fields.push(self.parse_type()?);
            while self.lex.eat(&Token::Comma) {
                fields.push(self.parse_type()?);
            }
        }
        self.lex.expect(&Token::RBrace)?;
        if packed {
            self.lex.expect(&Token::RAngle)?;
        }
        Ok((fields, packed))
    }

    #[allow(dead_code)]
    fn parse_function_type(&mut self, ret: TypeId) -> Result<TypeId, ParseError> {
        self.lex.expect(&Token::LParen)?;
        let mut params = Vec::new();
        let mut variadic = false;
        if !matches!(self.lex.peek()?, Token::RParen) {
            if self.lex.eat(&Token::Ellipsis) {
                variadic = true;
            } else {
                params.push(self.parse_type()?);
                while self.lex.eat(&Token::Comma) {
                    if self.lex.eat(&Token::Ellipsis) {
                        variadic = true;
                        break;
                    }
                    params.push(self.parse_type()?);
                }
            }
        }
        self.lex.expect(&Token::RParen)?;
        Ok(self.ctx.mk_fn_type(ret, params, variadic))
    }

    // -----------------------------------------------------------------------
    // Function parsing
    // -----------------------------------------------------------------------

    fn parse_function(&mut self, is_declaration: bool) -> Result<(), ParseError> {
        if is_declaration {
            self.lex.expect_kw(&Keyword::Declare)?;
        } else {
            self.lex.expect_kw(&Keyword::Define)?;
        }

        let linkage = self.parse_optional_linkage();

        // Skip optional function attributes before return type.
        // (dso_local, etc. — we skip unknown bare words here)
        self.skip_fn_attrs()?;

        let ret_ty = self.parse_type()?;
        let name = self.lex.expect_global_ident()?;

        // Parse parameter list.
        self.lex.expect(&Token::LParen)?;
        let mut params: Vec<(TypeId, String)> = Vec::new();
        let mut variadic = false;
        if !matches!(self.lex.peek()?, Token::RParen) {
            if self.lex.eat(&Token::Ellipsis) {
                variadic = true;
            } else {
                let (ty, pname) = self.parse_param()?;
                params.push((ty, pname));
                while self.lex.eat(&Token::Comma) {
                    if self.lex.eat(&Token::Ellipsis) {
                        variadic = true;
                        break;
                    }
                    let (ty, pname) = self.parse_param()?;
                    params.push((ty, pname));
                }
            }
        }
        self.lex.expect(&Token::RParen)?;

        // Skip trailing function attributes (e.g. #0, nounwind, ...).
        self.skip_trailing_fn_attrs()?;

        let fn_ty =
            self.ctx
                .mk_fn_type(ret_ty, params.iter().map(|(ty, _)| *ty).collect(), variadic);
        let args: Vec<Argument> = params
            .into_iter()
            .enumerate()
            .map(|(i, (ty, nm))| Argument {
                name: nm,
                ty,
                index: i as u32,
            })
            .collect();

        // Reset local state for this function.
        self.locals.clear();
        self.unnamed.clear();
        self.pending_blocks.clear();

        // Populate arg name table.
        for (i, arg) in args.iter().enumerate() {
            let vref = ValueRef::Argument(ArgId(i as u32));
            if !arg.name.is_empty() {
                self.locals.insert(arg.name.clone(), vref);
            }
        }

        if is_declaration {
            let func = Function::new_declaration(&name, fn_ty, args, linkage);
            let idx = self.module.add_function(func);
            self.current_func = Some(idx.0 as usize);
            return Ok(());
        }

        // Parse body.
        let func = Function::new(&name, fn_ty, args, linkage);
        let idx = self.module.add_function(func);
        self.current_func = Some(idx.0 as usize);

        self.lex.expect(&Token::LBrace)?;
        loop {
            match self.lex.peek()? {
                Token::RBrace => {
                    self.lex.next()?;
                    break;
                }
                _ => {
                    self.parse_block()?;
                }
            }
        }

        Ok(())
    }

    fn parse_param(&mut self) -> Result<(TypeId, String), ParseError> {
        let ty = self.parse_type()?;
        // Optional parameter attributes (noundef, etc.) — skip.
        self.skip_param_attrs()?;
        // Optional name.
        let name = match self.lex.peek() {
            Ok(Token::LocalIdent(_)) => self.lex.expect_local_ident()?,
            _ => String::new(),
        };
        Ok((ty, name))
    }

    // -----------------------------------------------------------------------
    // Block parsing
    // -----------------------------------------------------------------------

    fn parse_block(&mut self) -> Result<(), ParseError> {
        // Block label: `name:` or bare (for entry).
        let bb_name = match self.lex.peek()? {
            Token::LocalIdent(_) => {
                let n = self.lex.expect_local_ident()?;
                // Optionally followed by `:`.
                self.lex.eat(&Token::Colon);
                n
            }
            Token::IntLit(n) => {
                let n = *n as u64;
                let s = n.to_string();
                self.lex.next()?;
                self.lex.eat(&Token::Colon);
                s
            }
            _ => "entry".to_string(),
        };

        let fid = self
            .current_func
            .ok_or_else(|| self.err("block outside function"))?;
        let func = &mut self.module.functions[fid];

        // Reuse pre-allocated BlockId if this block was forward-referenced.
        let bid = if let Some(&existing) = self.pending_blocks.get(&bb_name) {
            existing
        } else {
            let bb = BasicBlock::new(&bb_name);
            let bid = func.add_block(bb);
            self.pending_blocks.insert(bb_name.clone(), bid);
            bid
        };

        // Make sure the BasicBlock exists with the right name.
        // (If it was a forward ref, the block already exists.)

        // Register block label as local value for branch targets.
        // (We don't represent labels as ValueRef currently, but names are used for br targets.)

        // Parse instructions until we see another block label or `}`.
        loop {
            match self.lex.peek()? {
                Token::RBrace => break,
                Token::LocalIdent(_) | Token::IntLit(_) => {
                    // If the current block already has a terminator, any ident or
                    // integer token must be the start of the next block label.
                    if self.block_is_complete(bid) {
                        break;
                    }
                    self.parse_instruction(bid)?;
                }
                _ => {
                    self.parse_instruction(bid)?;
                }
            }
        }

        Ok(())
    }

    fn block_is_complete(&self, bid: BlockId) -> bool {
        let fid = match self.current_func {
            Some(f) => f,
            None => return false,
        };
        self.module.functions[fid].block(bid).is_complete()
    }

    // -----------------------------------------------------------------------
    // Instruction parsing
    // -----------------------------------------------------------------------

    fn parse_instruction(&mut self, bid: BlockId) -> Result<(), ParseError> {
        let fid = self
            .current_func
            .ok_or_else(|| self.err("instruction outside function"))?;

        // Parse optional result assignment: `%name = ` or `%N = `.
        let (result_name, result_slot) = match self.lex.peek()? {
            Token::LocalIdent(_) => {
                // Peek ahead: if next is `=`, this is an assignment.
                // We consume the ident and then check for `=`.
                let n = self.lex.expect_local_ident()?;
                if self.lex.eat(&Token::Equal) {
                    // Named result.
                    (Some(n), None)
                } else {
                    // Bare word (shouldn't happen in valid IR).
                    return Err(self.err(format!("unexpected identifier '{}'", n)));
                }
            }
            Token::IntLit(slot) => {
                let slot = *slot as u64;
                self.lex.next()?;
                if self.lex.eat(&Token::Equal) {
                    (None, Some(slot))
                } else {
                    return Err(self.err("expected '=' after slot number"));
                }
            }
            _ => (None, None),
        };

        let (kind, ty) = self.parse_instr_kind()?;
        let is_term = kind.is_terminator();

        let instr_name = result_name.clone();
        let instr = Instruction::new(instr_name, ty, kind);
        let iid = self.module.functions[fid].alloc_instr(instr);

        if is_term {
            self.module.functions[fid]
                .block_mut(bid)
                .set_terminator(iid);
        } else {
            self.module.functions[fid].block_mut(bid).append_instr(iid);
        }

        let vref = ValueRef::Instruction(iid);
        if let Some(name) = result_name {
            self.locals.insert(name, vref);
        } else if let Some(slot) = result_slot {
            self.unnamed.insert(slot, vref);
        }

        Ok(())
    }

    fn parse_instr_kind(&mut self) -> Result<(InstrKind, TypeId), ParseError> {
        match self.lex.peek()? {
            // --- Integer arithmetic ---
            Token::Kw(Keyword::Add) => {
                self.lex.next()?;
                let flags = self.parse_int_arith_flags();
                let (lhs, ty) = self.parse_typed_value()?;
                self.lex.expect(&Token::Comma)?;
                let rhs = self.parse_value(ty)?;
                Ok((InstrKind::Add { flags, lhs, rhs }, ty))
            }
            Token::Kw(Keyword::Sub) => {
                self.lex.next()?;
                let flags = self.parse_int_arith_flags();
                let (lhs, ty) = self.parse_typed_value()?;
                self.lex.expect(&Token::Comma)?;
                let rhs = self.parse_value(ty)?;
                Ok((InstrKind::Sub { flags, lhs, rhs }, ty))
            }
            Token::Kw(Keyword::Mul) => {
                self.lex.next()?;
                let flags = self.parse_int_arith_flags();
                let (lhs, ty) = self.parse_typed_value()?;
                self.lex.expect(&Token::Comma)?;
                let rhs = self.parse_value(ty)?;
                Ok((InstrKind::Mul { flags, lhs, rhs }, ty))
            }
            Token::Kw(Keyword::Udiv) => {
                self.lex.next()?;
                let exact = self.lex.eat_kw(Keyword::Exact);
                let (lhs, ty) = self.parse_typed_value()?;
                self.lex.expect(&Token::Comma)?;
                let rhs = self.parse_value(ty)?;
                Ok((InstrKind::UDiv { exact, lhs, rhs }, ty))
            }
            Token::Kw(Keyword::Sdiv) => {
                self.lex.next()?;
                let exact = self.lex.eat_kw(Keyword::Exact);
                let (lhs, ty) = self.parse_typed_value()?;
                self.lex.expect(&Token::Comma)?;
                let rhs = self.parse_value(ty)?;
                Ok((InstrKind::SDiv { exact, lhs, rhs }, ty))
            }
            Token::Kw(Keyword::Urem) => {
                self.lex.next()?;
                let (lhs, ty) = self.parse_typed_value()?;
                self.lex.expect(&Token::Comma)?;
                let rhs = self.parse_value(ty)?;
                Ok((InstrKind::URem { lhs, rhs }, ty))
            }
            Token::Kw(Keyword::Srem) => {
                self.lex.next()?;
                let (lhs, ty) = self.parse_typed_value()?;
                self.lex.expect(&Token::Comma)?;
                let rhs = self.parse_value(ty)?;
                Ok((InstrKind::SRem { lhs, rhs }, ty))
            }
            // --- Bitwise ---
            Token::Kw(Keyword::And) => {
                self.lex.next()?;
                let (lhs, ty) = self.parse_typed_value()?;
                self.lex.expect(&Token::Comma)?;
                let rhs = self.parse_value(ty)?;
                Ok((InstrKind::And { lhs, rhs }, ty))
            }
            Token::Kw(Keyword::Or) => {
                self.lex.next()?;
                let (lhs, ty) = self.parse_typed_value()?;
                self.lex.expect(&Token::Comma)?;
                let rhs = self.parse_value(ty)?;
                Ok((InstrKind::Or { lhs, rhs }, ty))
            }
            Token::Kw(Keyword::Xor) => {
                self.lex.next()?;
                let (lhs, ty) = self.parse_typed_value()?;
                self.lex.expect(&Token::Comma)?;
                let rhs = self.parse_value(ty)?;
                Ok((InstrKind::Xor { lhs, rhs }, ty))
            }
            Token::Kw(Keyword::Shl) => {
                self.lex.next()?;
                let flags = self.parse_int_arith_flags();
                let (lhs, ty) = self.parse_typed_value()?;
                self.lex.expect(&Token::Comma)?;
                let rhs = self.parse_value(ty)?;
                Ok((InstrKind::Shl { flags, lhs, rhs }, ty))
            }
            Token::Kw(Keyword::Lshr) => {
                self.lex.next()?;
                let exact = self.lex.eat_kw(Keyword::Exact);
                let (lhs, ty) = self.parse_typed_value()?;
                self.lex.expect(&Token::Comma)?;
                let rhs = self.parse_value(ty)?;
                Ok((InstrKind::LShr { exact, lhs, rhs }, ty))
            }
            Token::Kw(Keyword::Ashr) => {
                self.lex.next()?;
                let exact = self.lex.eat_kw(Keyword::Exact);
                let (lhs, ty) = self.parse_typed_value()?;
                self.lex.expect(&Token::Comma)?;
                let rhs = self.parse_value(ty)?;
                Ok((InstrKind::AShr { exact, lhs, rhs }, ty))
            }
            // --- FP arithmetic ---
            Token::Kw(Keyword::Fadd) => {
                self.lex.next()?;
                let flags = self.parse_fast_math_flags();
                let (lhs, ty) = self.parse_typed_value()?;
                self.lex.expect(&Token::Comma)?;
                let rhs = self.parse_value(ty)?;
                Ok((InstrKind::FAdd { flags, lhs, rhs }, ty))
            }
            Token::Kw(Keyword::Fsub) => {
                self.lex.next()?;
                let flags = self.parse_fast_math_flags();
                let (lhs, ty) = self.parse_typed_value()?;
                self.lex.expect(&Token::Comma)?;
                let rhs = self.parse_value(ty)?;
                Ok((InstrKind::FSub { flags, lhs, rhs }, ty))
            }
            Token::Kw(Keyword::Fmul) => {
                self.lex.next()?;
                let flags = self.parse_fast_math_flags();
                let (lhs, ty) = self.parse_typed_value()?;
                self.lex.expect(&Token::Comma)?;
                let rhs = self.parse_value(ty)?;
                Ok((InstrKind::FMul { flags, lhs, rhs }, ty))
            }
            Token::Kw(Keyword::Fdiv) => {
                self.lex.next()?;
                let flags = self.parse_fast_math_flags();
                let (lhs, ty) = self.parse_typed_value()?;
                self.lex.expect(&Token::Comma)?;
                let rhs = self.parse_value(ty)?;
                Ok((InstrKind::FDiv { flags, lhs, rhs }, ty))
            }
            Token::Kw(Keyword::Frem) => {
                self.lex.next()?;
                let flags = self.parse_fast_math_flags();
                let (lhs, ty) = self.parse_typed_value()?;
                self.lex.expect(&Token::Comma)?;
                let rhs = self.parse_value(ty)?;
                Ok((InstrKind::FRem { flags, lhs, rhs }, ty))
            }
            Token::Kw(Keyword::Fneg) => {
                self.lex.next()?;
                let flags = self.parse_fast_math_flags();
                let (operand, ty) = self.parse_typed_value()?;
                Ok((InstrKind::FNeg { flags, operand }, ty))
            }
            // --- Comparisons ---
            Token::Kw(Keyword::Icmp) => {
                self.lex.next()?;
                let pred = self.parse_int_pred()?;
                let (lhs, _ty) = self.parse_typed_value()?;
                self.lex.expect(&Token::Comma)?;
                let rhs = self.parse_value(_ty)?;
                let i1 = self.ctx.i1_ty;
                Ok((InstrKind::ICmp { pred, lhs, rhs }, i1))
            }
            Token::Kw(Keyword::Fcmp) => {
                self.lex.next()?;
                let flags = self.parse_fast_math_flags();
                let pred = self.parse_float_pred()?;
                let (lhs, _ty) = self.parse_typed_value()?;
                self.lex.expect(&Token::Comma)?;
                let rhs = self.parse_value(_ty)?;
                let i1 = self.ctx.i1_ty;
                Ok((
                    InstrKind::FCmp {
                        flags,
                        pred,
                        lhs,
                        rhs,
                    },
                    i1,
                ))
            }
            // --- Memory ---
            Token::Kw(Keyword::Alloca) => {
                self.lex.next()?;
                let alloc_ty = self.parse_type()?;
                let num_elements = if self.lex.eat(&Token::Comma) {
                    match self.lex.peek()? {
                        Token::Kw(Keyword::Align) => None,
                        _ => {
                            let (ne, _) = self.parse_typed_value()?;
                            Some(ne)
                        }
                    }
                } else {
                    None
                };
                let align = self.parse_optional_align()?;
                let ptr_ty = self.ctx.ptr_ty;
                Ok((
                    InstrKind::Alloca {
                        alloc_ty,
                        num_elements,
                        align,
                    },
                    ptr_ty,
                ))
            }
            Token::Kw(Keyword::Load) => {
                self.lex.next()?;
                let volatile = self.lex.eat_kw(Keyword::Volatile);
                let ty = self.parse_type()?;
                self.lex.expect(&Token::Comma)?;
                let (_ptr_ty, ptr) = {
                    let ptype = self.parse_type()?;
                    (ptype, self.parse_value(ptype)?)
                };
                let align = self.parse_optional_align()?;
                Ok((
                    InstrKind::Load {
                        ty,
                        ptr,
                        align,
                        volatile,
                    },
                    ty,
                ))
            }
            Token::Kw(Keyword::Store) => {
                self.lex.next()?;
                let volatile = self.lex.eat_kw(Keyword::Volatile);
                let (val, _val_ty) = self.parse_typed_value()?;
                self.lex.expect(&Token::Comma)?;
                let ptr_ty2 = self.parse_type()?;
                let ptr = self.parse_value(ptr_ty2)?;
                let align = self.parse_optional_align()?;
                let void_ty = self.ctx.void_ty;
                Ok((
                    InstrKind::Store {
                        val,
                        ptr,
                        align,
                        volatile,
                    },
                    void_ty,
                ))
            }
            Token::Kw(Keyword::Getelementptr) => {
                self.lex.next()?;
                let inbounds = self.lex.eat_kw(Keyword::Inbounds);
                let base_ty = self.parse_type()?;
                self.lex.expect(&Token::Comma)?;
                let ptr_ty2 = self.parse_type()?;
                let ptr = self.parse_value(ptr_ty2)?;
                let mut indices = Vec::new();
                while self.lex.eat(&Token::Comma) {
                    let (idx, _) = self.parse_typed_value()?;
                    indices.push(idx);
                }
                let ptr_ty = self.ctx.ptr_ty;
                Ok((
                    InstrKind::GetElementPtr {
                        inbounds,
                        base_ty,
                        ptr,
                        indices,
                    },
                    ptr_ty,
                ))
            }
            // --- Casts ---
            Token::Kw(Keyword::Trunc) => {
                self.lex.next()?;
                let (val, _) = self.parse_typed_value()?;
                self.lex.expect_kw(&Keyword::To)?;
                let to = self.parse_type()?;
                Ok((InstrKind::Trunc { val, to }, to))
            }
            Token::Kw(Keyword::Zext) => {
                self.lex.next()?;
                let (val, _) = self.parse_typed_value()?;
                self.lex.expect_kw(&Keyword::To)?;
                let to = self.parse_type()?;
                Ok((InstrKind::ZExt { val, to }, to))
            }
            Token::Kw(Keyword::Sext) => {
                self.lex.next()?;
                let (val, _) = self.parse_typed_value()?;
                self.lex.expect_kw(&Keyword::To)?;
                let to = self.parse_type()?;
                Ok((InstrKind::SExt { val, to }, to))
            }
            Token::Kw(Keyword::Fptrunc) => {
                self.lex.next()?;
                let (val, _) = self.parse_typed_value()?;
                self.lex.expect_kw(&Keyword::To)?;
                let to = self.parse_type()?;
                Ok((InstrKind::FPTrunc { val, to }, to))
            }
            Token::Kw(Keyword::Fpext) => {
                self.lex.next()?;
                let (val, _) = self.parse_typed_value()?;
                self.lex.expect_kw(&Keyword::To)?;
                let to = self.parse_type()?;
                Ok((InstrKind::FPExt { val, to }, to))
            }
            Token::Kw(Keyword::Fptoui) => {
                self.lex.next()?;
                let (val, _) = self.parse_typed_value()?;
                self.lex.expect_kw(&Keyword::To)?;
                let to = self.parse_type()?;
                Ok((InstrKind::FPToUI { val, to }, to))
            }
            Token::Kw(Keyword::Fptosi) => {
                self.lex.next()?;
                let (val, _) = self.parse_typed_value()?;
                self.lex.expect_kw(&Keyword::To)?;
                let to = self.parse_type()?;
                Ok((InstrKind::FPToSI { val, to }, to))
            }
            Token::Kw(Keyword::Uitofp) => {
                self.lex.next()?;
                let (val, _) = self.parse_typed_value()?;
                self.lex.expect_kw(&Keyword::To)?;
                let to = self.parse_type()?;
                Ok((InstrKind::UIToFP { val, to }, to))
            }
            Token::Kw(Keyword::Sitofp) => {
                self.lex.next()?;
                let (val, _) = self.parse_typed_value()?;
                self.lex.expect_kw(&Keyword::To)?;
                let to = self.parse_type()?;
                Ok((InstrKind::SIToFP { val, to }, to))
            }
            Token::Kw(Keyword::Ptrtoint) => {
                self.lex.next()?;
                let (val, _) = self.parse_typed_value()?;
                self.lex.expect_kw(&Keyword::To)?;
                let to = self.parse_type()?;
                Ok((InstrKind::PtrToInt { val, to }, to))
            }
            Token::Kw(Keyword::Inttoptr) => {
                self.lex.next()?;
                let (val, _) = self.parse_typed_value()?;
                self.lex.expect_kw(&Keyword::To)?;
                let to = self.parse_type()?;
                Ok((InstrKind::IntToPtr { val, to }, to))
            }
            Token::Kw(Keyword::Bitcast) => {
                self.lex.next()?;
                let (val, _) = self.parse_typed_value()?;
                self.lex.expect_kw(&Keyword::To)?;
                let to = self.parse_type()?;
                Ok((InstrKind::BitCast { val, to }, to))
            }
            Token::Kw(Keyword::Addrspacecast) => {
                self.lex.next()?;
                let (val, _) = self.parse_typed_value()?;
                self.lex.expect_kw(&Keyword::To)?;
                let to = self.parse_type()?;
                Ok((InstrKind::AddrSpaceCast { val, to }, to))
            }
            // --- Misc ---
            Token::Kw(Keyword::Select) => {
                self.lex.next()?;
                let (cond, _) = self.parse_typed_value()?;
                self.lex.expect(&Token::Comma)?;
                let (then_val, ty) = self.parse_typed_value()?;
                self.lex.expect(&Token::Comma)?;
                let else_val = self.parse_value(ty)?;
                Ok((
                    InstrKind::Select {
                        cond,
                        then_val,
                        else_val,
                    },
                    ty,
                ))
            }
            Token::Kw(Keyword::Phi) => {
                self.lex.next()?;
                let ty = self.parse_type()?;
                let mut incoming = Vec::new();
                loop {
                    // [ value, %label ]
                    self.lex.expect(&Token::LBracket)?;
                    let val = self.parse_value(ty)?;
                    self.lex.expect(&Token::Comma)?;
                    let block_name = self.lex.expect_local_ident()?;
                    let bid = self.get_or_create_block(&block_name)?;
                    self.lex.expect(&Token::RBracket)?;
                    incoming.push((val, bid));
                    if !self.lex.eat(&Token::Comma) {
                        break;
                    }
                }
                Ok((InstrKind::Phi { ty, incoming }, ty))
            }
            Token::Kw(Keyword::Extractvalue) => {
                self.lex.next()?;
                let (aggregate, agg_ty) = self.parse_typed_value()?;
                let mut indices = Vec::new();
                while self.lex.eat(&Token::Comma) {
                    let idx = self.lex.expect_uint_lit()? as u32;
                    indices.push(idx);
                }
                // Result type is field type — approximate as the aggregate type for now.
                Ok((InstrKind::ExtractValue { aggregate, indices }, agg_ty))
            }
            Token::Kw(Keyword::Insertvalue) => {
                self.lex.next()?;
                let (aggregate, agg_ty) = self.parse_typed_value()?;
                self.lex.expect(&Token::Comma)?;
                let (val, _val_ty) = self.parse_typed_value()?;
                let mut indices = Vec::new();
                while self.lex.eat(&Token::Comma) {
                    let idx = self.lex.expect_uint_lit()? as u32;
                    indices.push(idx);
                }
                Ok((
                    InstrKind::InsertValue {
                        aggregate,
                        val,
                        indices,
                    },
                    agg_ty,
                ))
            }
            Token::Kw(Keyword::Extractelement) => {
                self.lex.next()?;
                let (vec, vec_ty) = self.parse_typed_value()?;
                self.lex.expect(&Token::Comma)?;
                let (idx, _) = self.parse_typed_value()?;
                // Result type is element type — approximate.
                Ok((InstrKind::ExtractElement { vec, idx }, vec_ty))
            }
            Token::Kw(Keyword::Insertelement) => {
                self.lex.next()?;
                let (vec, vec_ty) = self.parse_typed_value()?;
                self.lex.expect(&Token::Comma)?;
                let (val, _) = self.parse_typed_value()?;
                self.lex.expect(&Token::Comma)?;
                let (idx, _) = self.parse_typed_value()?;
                Ok((InstrKind::InsertElement { vec, val, idx }, vec_ty))
            }
            Token::Kw(Keyword::Shufflevector) => {
                self.lex.next()?;
                let (v1, vec_ty) = self.parse_typed_value()?;
                self.lex.expect(&Token::Comma)?;
                let (v2, _) = self.parse_typed_value()?;
                self.lex.expect(&Token::Comma)?;
                // Mask: <i32 N, i32 M, ...> or undef
                let mask = self.parse_shuffle_mask()?;
                Ok((InstrKind::ShuffleVector { v1, v2, mask }, vec_ty))
            }
            // --- Call ---
            Token::Kw(Keyword::Call)
            | Token::Kw(Keyword::Tail)
            | Token::Kw(Keyword::Musttail)
            | Token::Kw(Keyword::Notail) => {
                let tail = match self.lex.peek()? {
                    Token::Kw(Keyword::Tail) => {
                        self.lex.next()?;
                        TailCallKind::Tail
                    }
                    Token::Kw(Keyword::Musttail) => {
                        self.lex.next()?;
                        TailCallKind::MustTail
                    }
                    Token::Kw(Keyword::Notail) => {
                        self.lex.next()?;
                        TailCallKind::NoTail
                    }
                    _ => TailCallKind::None,
                };
                self.lex.expect_kw(&Keyword::Call)?;
                // Optional fast-math flags.
                let _fmf = self.parse_fast_math_flags();
                // Return type.
                let ret_ty = self.parse_type()?;
                // Callee.
                let callee = match self.lex.peek()? {
                    Token::GlobalIdent(_) => {
                        let gname = self.lex.expect_global_ident()?;
                        self.resolve_global_ref(&gname)?
                    }
                    Token::LocalIdent(_) => {
                        let lname = self.lex.expect_local_ident()?;
                        self.resolve_local(&lname)?
                    }
                    _ => return Err(self.err("expected callee name")),
                };
                // Arg list.
                self.lex.expect(&Token::LParen)?;
                let mut args = Vec::new();
                if !matches!(self.lex.peek()?, Token::RParen) {
                    let (a, _) = self.parse_typed_value()?;
                    args.push(a);
                    while self.lex.eat(&Token::Comma) {
                        if self.lex.eat(&Token::Ellipsis) {
                            break;
                        }
                        let (a, _) = self.parse_typed_value()?;
                        args.push(a);
                    }
                }
                self.lex.expect(&Token::RParen)?;
                // Build a function type from what we know.
                let param_tys: Vec<TypeId> = args.iter().map(|a| self.type_of_vref(*a)).collect();
                let callee_ty = self.ctx.mk_fn_type(ret_ty, param_tys, false);
                Ok((
                    InstrKind::Call {
                        tail,
                        callee_ty,
                        callee,
                        args,
                    },
                    ret_ty,
                ))
            }
            // --- Terminators ---
            Token::Kw(Keyword::Ret) => {
                self.lex.next()?;
                let void_ty = self.ctx.void_ty;
                if self.lex.eat_kw(Keyword::Void) {
                    Ok((InstrKind::Ret { val: None }, void_ty))
                } else {
                    let (val, _) = self.parse_typed_value()?;
                    Ok((InstrKind::Ret { val: Some(val) }, void_ty))
                }
            }
            Token::Kw(Keyword::Br) => {
                self.lex.next()?;
                let void_ty = self.ctx.void_ty;
                // `br label %dest` or `br i1 %cond, label %then, label %else`
                match self.lex.peek()? {
                    Token::Kw(Keyword::Label) => {
                        self.lex.next()?;
                        let dest_name = self.lex.expect_local_ident()?;
                        let dest = self.get_or_create_block(&dest_name)?;
                        Ok((InstrKind::Br { dest }, void_ty))
                    }
                    _ => {
                        let (cond, _) = self.parse_typed_value()?;
                        self.lex.expect(&Token::Comma)?;
                        self.lex.expect_kw(&Keyword::Label)?;
                        let then_name = self.lex.expect_local_ident()?;
                        let then_dest = self.get_or_create_block(&then_name)?;
                        self.lex.expect(&Token::Comma)?;
                        self.lex.expect_kw(&Keyword::Label)?;
                        let else_name = self.lex.expect_local_ident()?;
                        let else_dest = self.get_or_create_block(&else_name)?;
                        Ok((
                            InstrKind::CondBr {
                                cond,
                                then_dest,
                                else_dest,
                            },
                            void_ty,
                        ))
                    }
                }
            }
            Token::Kw(Keyword::Switch) => {
                self.lex.next()?;
                let void_ty = self.ctx.void_ty;
                let (val, val_ty) = self.parse_typed_value()?;
                self.lex.expect(&Token::Comma)?;
                self.lex.expect_kw(&Keyword::Label)?;
                let default_name = self.lex.expect_local_ident()?;
                let default = self.get_or_create_block(&default_name)?;
                self.lex.expect(&Token::LBracket)?;
                let mut cases = Vec::new();
                while !matches!(self.lex.peek()?, Token::RBracket) {
                    let case_val = self.parse_value(val_ty)?;
                    self.lex.expect(&Token::Comma)?;
                    self.lex.expect_kw(&Keyword::Label)?;
                    let dest_name = self.lex.expect_local_ident()?;
                    let dest = self.get_or_create_block(&dest_name)?;
                    cases.push((case_val, dest));
                }
                self.lex.expect(&Token::RBracket)?;
                Ok((
                    InstrKind::Switch {
                        val,
                        default,
                        cases,
                    },
                    void_ty,
                ))
            }
            Token::Kw(Keyword::Unreachable) => {
                self.lex.next()?;
                let void_ty = self.ctx.void_ty;
                Ok((InstrKind::Unreachable, void_ty))
            }
            _ => {
                let t = self.lex.next()?;
                Err(self.err(format!("unknown instruction opcode: {:?}", t)))
            }
        }
    }

    // -----------------------------------------------------------------------
    // Operand helpers
    // -----------------------------------------------------------------------

    fn parse_typed_value(&mut self) -> Result<(ValueRef, TypeId), ParseError> {
        let ty = self.parse_type()?;
        let val = self.parse_value(ty)?;
        Ok((val, ty))
    }

    fn parse_value(&mut self, ty: TypeId) -> Result<ValueRef, ParseError> {
        match self.lex.peek()? {
            Token::LocalIdent(_) => {
                let name = self.lex.expect_local_ident()?;
                self.resolve_local(&name)
            }
            Token::GlobalIdent(_) => {
                let name = self.lex.expect_global_ident()?;
                self.resolve_global_ref(&name)
            }
            Token::IntLit(_) | Token::UIntLit(_) => {
                let n = self.lex.expect_uint_lit()?;
                let c = self.ctx.const_int(ty, n);
                Ok(ValueRef::Constant(c))
            }
            Token::FloatLit(_) => {
                if let Token::FloatLit(f) = self.lex.next()? {
                    let bits = f.to_bits();
                    let c = self.ctx.const_float(ty, bits);
                    Ok(ValueRef::Constant(c))
                } else {
                    unreachable!()
                }
            }
            Token::Kw(Keyword::Undef) => {
                self.lex.next()?;
                Ok(ValueRef::Constant(self.ctx.const_undef(ty)))
            }
            Token::Kw(Keyword::Poison) => {
                self.lex.next()?;
                Ok(ValueRef::Constant(self.ctx.const_poison(ty)))
            }
            Token::Kw(Keyword::Null) => {
                self.lex.next()?;
                Ok(ValueRef::Constant(self.ctx.const_null(ty)))
            }
            Token::Kw(Keyword::Zeroinitializer) => {
                self.lex.next()?;
                Ok(ValueRef::Constant(self.ctx.const_zero(ty)))
            }
            Token::Kw(Keyword::True) => {
                self.lex.next()?;
                let i1 = self.ctx.i1_ty;
                Ok(ValueRef::Constant(self.ctx.const_int(i1, 1)))
            }
            Token::Kw(Keyword::False) => {
                self.lex.next()?;
                let i1 = self.ctx.i1_ty;
                Ok(ValueRef::Constant(self.ctx.const_int(i1, 0)))
            }
            _ => {
                let t = self.lex.next()?;
                Err(self.err(format!("expected value, got {:?}", t)))
            }
        }
    }

    fn parse_constant(&mut self, ty: TypeId) -> Result<ConstId, ParseError> {
        let vref = self.parse_value(ty)?;
        match vref {
            ValueRef::Constant(c) => Ok(c),
            _ => Err(self.err("expected constant")),
        }
    }

    fn resolve_local(&self, name: &str) -> Result<ValueRef, ParseError> {
        if let Some(&v) = self.locals.get(name) {
            return Ok(v);
        }
        // Try numeric slot.
        if let Ok(slot) = name.parse::<u64>() {
            if let Some(&v) = self.unnamed.get(&slot) {
                return Ok(v);
            }
        }
        Err(ParseError {
            line: self.lex.current_line(),
            col: self.lex.current_col(),
            message: format!("undefined local value '%{}'", name),
        })
    }

    fn resolve_global_ref(&mut self, name: &str) -> Result<ValueRef, ParseError> {
        let ptr_ty = self.ctx.ptr_ty;
        // Look up in module globals first.
        if let Some(gid) = self.module.get_global_id(name) {
            let c = self.ctx.push_const(ConstantData::GlobalRef {
                ty: ptr_ty,
                id: gid,
                name: name.to_string(),
            });
            return Ok(ValueRef::Constant(c));
        }
        // Functions are also referenced by @name (as function pointers / callees).
        // Use GlobalId::MAX as a sentinel meaning "function reference".
        if self.module.get_function_id(name).is_some() {
            let c = self.ctx.push_const(ConstantData::GlobalRef {
                ty: ptr_ty,
                id: GlobalId(u32::MAX),
                name: name.to_string(),
            });
            return Ok(ValueRef::Constant(c));
        }
        // Forward/unknown reference — record name for future resolution.
        let c = self.ctx.push_const(ConstantData::GlobalRef {
            ty: ptr_ty,
            id: GlobalId(u32::MAX),
            name: name.to_string(),
        });
        Ok(ValueRef::Constant(c))
    }

    fn type_of_vref(&self, vref: ValueRef) -> TypeId {
        if let Some(fid) = self.current_func {
            if fid < self.module.functions.len() {
                let func = &self.module.functions[fid];
                match vref {
                    ValueRef::Instruction(id) if (id.0 as usize) < func.instructions.len() => {
                        return func.instr(id).ty;
                    }
                    ValueRef::Argument(id) if (id.0 as usize) < func.args.len() => {
                        return func.arg(id).ty;
                    }
                    _ => {}
                }
            }
        }
        match vref {
            ValueRef::Constant(c) => self.ctx.type_of_const(c),
            _ => self.ctx.ptr_ty,
        }
    }

    fn get_or_create_block(&mut self, name: &str) -> Result<BlockId, ParseError> {
        let fid = self
            .current_func
            .ok_or_else(|| self.err("block reference outside function"))?;
        if let Some(&bid) = self.pending_blocks.get(name) {
            return Ok(bid);
        }
        let bb = BasicBlock::new(name);
        let bid = self.module.functions[fid].add_block(bb);
        self.pending_blocks.insert(name.to_string(), bid);
        Ok(bid)
    }

    // -----------------------------------------------------------------------
    // Flag helpers
    // -----------------------------------------------------------------------

    fn parse_int_arith_flags(&mut self) -> IntArithFlags {
        let mut flags = IntArithFlags::default();
        loop {
            if self.lex.eat_kw(Keyword::Nuw) {
                flags.nuw = true;
            } else if self.lex.eat_kw(Keyword::Nsw) {
                flags.nsw = true;
            } else {
                break;
            }
        }
        flags
    }

    fn parse_fast_math_flags(&mut self) -> FastMathFlags {
        let mut f = FastMathFlags::default();
        loop {
            if self.lex.eat_kw(Keyword::Fast) {
                f.fast = true;
                break;
            } else if self.lex.eat_kw(Keyword::Nnan) {
                f.nnan = true;
            } else if self.lex.eat_kw(Keyword::Ninf) {
                f.ninf = true;
            } else if self.lex.eat_kw(Keyword::Nsz) {
                f.nsz = true;
            } else if self.lex.eat_kw(Keyword::Arcp) {
                f.arcp = true;
            } else if self.lex.eat_kw(Keyword::Contract) {
                f.contract = true;
            } else if self.lex.eat_kw(Keyword::Afn) {
                f.afn = true;
            } else if self.lex.eat_kw(Keyword::Reassoc) {
                f.reassoc = true;
            } else {
                break;
            }
        }
        f
    }

    fn parse_int_pred(&mut self) -> Result<IntPredicate, ParseError> {
        match self.lex.next()? {
            Token::Kw(Keyword::Eq) => Ok(IntPredicate::Eq),
            Token::Kw(Keyword::Ne) => Ok(IntPredicate::Ne),
            Token::Kw(Keyword::Ugt) => Ok(IntPredicate::Ugt),
            Token::Kw(Keyword::Uge) => Ok(IntPredicate::Uge),
            Token::Kw(Keyword::Ult) => Ok(IntPredicate::Ult),
            Token::Kw(Keyword::Ule) => Ok(IntPredicate::Ule),
            Token::Kw(Keyword::Sgt) => Ok(IntPredicate::Sgt),
            Token::Kw(Keyword::Sge) => Ok(IntPredicate::Sge),
            Token::Kw(Keyword::Slt) => Ok(IntPredicate::Slt),
            Token::Kw(Keyword::Sle) => Ok(IntPredicate::Sle),
            t => Err(self.err(format!("expected icmp predicate, got {:?}", t))),
        }
    }

    fn parse_float_pred(&mut self) -> Result<FloatPredicate, ParseError> {
        match self.lex.next()? {
            Token::Kw(Keyword::False) => Ok(FloatPredicate::False),
            Token::Kw(Keyword::Oeq) => Ok(FloatPredicate::Oeq),
            Token::Kw(Keyword::Ogt) => Ok(FloatPredicate::Ogt),
            Token::Kw(Keyword::Oge) => Ok(FloatPredicate::Oge),
            Token::Kw(Keyword::Olt) => Ok(FloatPredicate::Olt),
            Token::Kw(Keyword::Ole) => Ok(FloatPredicate::Ole),
            Token::Kw(Keyword::One) => Ok(FloatPredicate::One),
            Token::Kw(Keyword::Ord) => Ok(FloatPredicate::Ord),
            Token::Kw(Keyword::Uno) => Ok(FloatPredicate::Uno),
            Token::Kw(Keyword::Ueq) => Ok(FloatPredicate::Ueq),
            Token::Kw(Keyword::Ugt) => Ok(FloatPredicate::Ugt),
            Token::Kw(Keyword::Uge) => Ok(FloatPredicate::Uge),
            Token::Kw(Keyword::Ult) => Ok(FloatPredicate::Ult),
            Token::Kw(Keyword::Ule) => Ok(FloatPredicate::Ule),
            Token::Kw(Keyword::Une) => Ok(FloatPredicate::Une),
            Token::Kw(Keyword::True) => Ok(FloatPredicate::True),
            t => Err(self.err(format!("expected fcmp predicate, got {:?}", t))),
        }
    }

    fn parse_optional_align(&mut self) -> Result<Option<u32>, ParseError> {
        if self.lex.eat(&Token::Comma) {
            self.lex.expect_kw(&Keyword::Align)?;
            let a = self.lex.expect_uint_lit()? as u32;
            Ok(Some(a))
        } else {
            Ok(None)
        }
    }

    fn parse_shuffle_mask(&mut self) -> Result<Vec<i32>, ParseError> {
        // Could be `<i32 0, i32 1>` or `undef`.
        if self.lex.eat_kw(Keyword::Undef) {
            return Ok(vec![]);
        }
        self.lex.expect(&Token::LAngle)?;
        let mut mask = Vec::new();
        loop {
            // Skip type.
            let _ = self.parse_type()?;
            let n = self.lex.expect_int_lit()? as i32;
            mask.push(n);
            if !self.lex.eat(&Token::Comma) {
                break;
            }
        }
        self.lex.expect(&Token::RAngle)?;
        Ok(mask)
    }

    // -----------------------------------------------------------------------
    // Skip helpers (for things we don't fully model yet)
    // -----------------------------------------------------------------------

    fn skip_fn_attrs(&mut self) -> Result<(), ParseError> {
        // Skip bare word attributes like `dso_local`, `nounwind`, etc.
        // that appear between `define`/`declare` and the return type.
        loop {
            match self.lex.peek()? {
                // These are valid type-starting tokens — stop skipping.
                Token::Kw(Keyword::Void)
                | Token::Kw(Keyword::Half)
                | Token::Kw(Keyword::Bfloat)
                | Token::Kw(Keyword::Float)
                | Token::Kw(Keyword::Double)
                | Token::Kw(Keyword::Fp128)
                | Token::Kw(Keyword::X86Fp80)
                | Token::Kw(Keyword::Ptr)
                | Token::IntType(_)
                | Token::LBracket
                | Token::LAngle
                | Token::LBrace
                | Token::LocalIdent(_) => break,
                // Linkage keywords already consumed.
                Token::Kw(Keyword::Private)
                | Token::Kw(Keyword::Internal)
                | Token::Kw(Keyword::External)
                | Token::Kw(Keyword::Weak) => {
                    self.lex.next()?;
                }
                Token::Hash => {
                    self.lex.next()?;
                    self.lex.next()?; // skip number
                }
                _ => break,
            }
        }
        Ok(())
    }

    fn skip_trailing_fn_attrs(&mut self) -> Result<(), ParseError> {
        // Skip `#N`, bare word attrs, etc. until `{` or EOF.
        loop {
            match self.lex.peek()? {
                Token::LBrace | Token::Eof => break,
                Token::Hash => {
                    self.lex.next()?;
                    self.lex.next()?;
                }
                Token::Bang => {
                    self.skip_metadata_line()?;
                    break;
                }
                _ => {
                    self.lex.next()?;
                }
            }
        }
        Ok(())
    }

    fn skip_param_attrs(&mut self) -> Result<(), ParseError> {
        // Skip param attrs like `noundef`, `nonnull`, `%N` alignment hints.
        loop {
            match self.lex.peek()? {
                Token::Comma | Token::RParen | Token::LocalIdent(_) => break,
                Token::Kw(Keyword::Align) => {
                    self.lex.next()?;
                    self.lex.next()?; // alignment number
                }
                Token::Hash => {
                    self.lex.next()?;
                    self.lex.next()?;
                }
                _ => {
                    self.lex.next()?;
                }
            }
        }
        Ok(())
    }

    fn skip_metadata_line(&mut self) -> Result<(), ParseError> {
        while !matches!(self.lex.peek()?, Token::Eof) {
            // Consume tokens until newline-ish heuristic: next top-level token.
            // We approximate: consume until we see a GlobalIdent, Kw(Define), etc.
            let tok = self.lex.next()?;
            if matches!(tok, Token::Eof) {
                break;
            }
            // Metadata lines end at things that look like module-level items.
            // Heuristic: stop when we've consumed something that looks complete.
            // For now we consume an entire "statement" by stopping at Eof.
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

pub fn parse(src: &str) -> Result<(Context, Module), ParseError> {
    let mut parser = Parser::new(src);
    parser.parse_module()?;
    Ok((parser.ctx, parser.module))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_empty_function() {
        let src = r#"
define void @empty() {
entry:
  ret void
}
"#;
        let (_ctx, module) = parse(src).expect("parse failed");
        assert_eq!(module.functions.len(), 1);
        let f = &module.functions[0];
        assert_eq!(f.name, "empty");
        assert!(!f.is_declaration);
        assert_eq!(f.blocks.len(), 1);
        assert_eq!(f.blocks[0].name, "entry");
    }

    #[test]
    fn parse_add_function() {
        let src = r#"
define i32 @add(i32 %a, i32 %b) {
entry:
  %result = add i32 %a, %b
  ret i32 %result
}
"#;
        let (_ctx, module) = parse(src).expect("parse failed");
        let f = &module.functions[0];
        assert_eq!(f.name, "add");
        assert_eq!(f.args.len(), 2);
        let bb = &f.blocks[0];
        assert_eq!(bb.body.len(), 1);
        assert!(bb.is_complete());
    }

    #[test]
    fn parse_declaration() {
        let src = "declare i32 @printf(ptr, ...)";
        let (_ctx, module) = parse(src).expect("parse failed");
        assert_eq!(module.functions.len(), 1);
        assert!(module.functions[0].is_declaration);
    }

    #[test]
    fn parse_global() {
        let src = "@x = global i32 42";
        let (_ctx, module) = parse(src).expect("parse failed");
        assert_eq!(module.globals.len(), 1);
        assert_eq!(module.globals[0].name, "x");
    }

    #[test]
    fn parse_target_metadata() {
        let src = r#"
source_filename = "test.c"
target triple = "x86_64-unknown-linux-gnu"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-f80:128-n8:16:32:64-S128"
"#;
        let (_ctx, module) = parse(src).expect("parse failed");
        assert_eq!(module.source_filename.as_deref(), Some("test.c"));
        assert_eq!(
            module.target_triple.as_deref(),
            Some("x86_64-unknown-linux-gnu")
        );
    }

    #[test]
    fn parse_cond_br() {
        let src = r#"
define void @check(i1 %cond) {
entry:
  br i1 %cond, label %then, label %else
then:
  ret void
else:
  ret void
}
"#;
        let (_ctx, module) = parse(src).expect("parse failed");
        let f = &module.functions[0];
        assert_eq!(f.blocks.len(), 3);
    }
}
