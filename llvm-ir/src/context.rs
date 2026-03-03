//! Context: interning tables for IR types and constants, plus all newtype index types.

use std::collections::HashMap;
use crate::types::{TypeData, FloatKind, StructType, FunctionType};
use crate::value::ConstantData;

// ---------------------------------------------------------------------------
// Newtype index types — all u32, Copy
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TypeId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FunctionId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BlockId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct InstrId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ArgId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ConstId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GlobalId(pub u32);

// ---------------------------------------------------------------------------
// Universal SSA value reference
// ---------------------------------------------------------------------------

/// A `Copy` reference to any SSA value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ValueRef {
    Instruction(InstrId),
    Argument(ArgId),
    Constant(ConstId),
    Global(GlobalId),
}

// ---------------------------------------------------------------------------
// Constant deduplication key (scalars only)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ConstantKey {
    Int(TypeId, u64),
    Float(TypeId, u64), // raw bits
    Null(TypeId),
    Undef(TypeId),
    Poison(TypeId),
    ZeroInitializer(TypeId),
}

// ---------------------------------------------------------------------------
// Context
// ---------------------------------------------------------------------------

pub struct Context {
    types: Vec<TypeData>,
    /// Structural type interning (anonymous types).
    type_map: HashMap<TypeData, TypeId>,
    /// Named struct lookup by name.
    named_struct_map: HashMap<String, TypeId>,
    /// Constant pool.
    pub constants: Vec<ConstantData>,
    const_map: HashMap<ConstantKey, ConstId>,

    // Pre-interned singletons.
    pub void_ty: TypeId,
    pub i1_ty: TypeId,
    pub i8_ty: TypeId,
    pub i16_ty: TypeId,
    pub i32_ty: TypeId,
    pub i64_ty: TypeId,
    pub f32_ty: TypeId,
    pub f64_ty: TypeId,
    pub ptr_ty: TypeId,
}

impl Context {
    pub fn new() -> Self {
        let mut ctx = Context {
            types: Vec::new(),
            type_map: HashMap::new(),
            named_struct_map: HashMap::new(),
            constants: Vec::new(),
            const_map: HashMap::new(),
            void_ty: TypeId(0),
            i1_ty: TypeId(0),
            i8_ty: TypeId(0),
            i16_ty: TypeId(0),
            i32_ty: TypeId(0),
            i64_ty: TypeId(0),
            f32_ty: TypeId(0),
            f64_ty: TypeId(0),
            ptr_ty: TypeId(0),
        };
        ctx.void_ty = ctx.intern_anon(TypeData::Void);
        ctx.i1_ty = ctx.intern_anon(TypeData::Integer(1));
        ctx.i8_ty = ctx.intern_anon(TypeData::Integer(8));
        ctx.i16_ty = ctx.intern_anon(TypeData::Integer(16));
        ctx.i32_ty = ctx.intern_anon(TypeData::Integer(32));
        ctx.i64_ty = ctx.intern_anon(TypeData::Integer(64));
        ctx.f32_ty = ctx.intern_anon(TypeData::Float(FloatKind::Single));
        ctx.f64_ty = ctx.intern_anon(TypeData::Float(FloatKind::Double));
        ctx.ptr_ty = ctx.intern_anon(TypeData::Pointer);
        ctx
    }

    /// Intern a non-named-struct type by structural equality.
    fn intern_anon(&mut self, td: TypeData) -> TypeId {
        if let Some(&id) = self.type_map.get(&td) {
            return id;
        }
        let id = TypeId(self.types.len() as u32);
        self.type_map.insert(td.clone(), id);
        self.types.push(td);
        id
    }

    // -----------------------------------------------------------------------
    // Type constructors
    // -----------------------------------------------------------------------

    pub fn mk_int(&mut self, bits: u32) -> TypeId {
        self.intern_anon(TypeData::Integer(bits))
    }

    pub fn mk_float(&mut self, kind: FloatKind) -> TypeId {
        self.intern_anon(TypeData::Float(kind))
    }

    pub fn mk_ptr(&mut self) -> TypeId {
        self.ptr_ty
    }

    pub fn mk_array(&mut self, element: TypeId, len: u64) -> TypeId {
        self.intern_anon(TypeData::Array { element, len })
    }

    pub fn mk_vector(&mut self, element: TypeId, len: u32, scalable: bool) -> TypeId {
        self.intern_anon(TypeData::Vector { element, len, scalable })
    }

    pub fn mk_fn_type(&mut self, ret: TypeId, params: Vec<TypeId>, variadic: bool) -> TypeId {
        self.intern_anon(TypeData::Function(FunctionType { ret, params, variadic }))
    }

    pub fn mk_struct_anon(&mut self, fields: Vec<TypeId>, packed: bool) -> TypeId {
        self.intern_anon(TypeData::Struct(StructType { name: None, fields, packed }))
    }

    /// Create or look up a named struct. If the name is new, an opaque (empty-body)
    /// struct is allocated. Call `define_struct_body` to fill in fields later.
    pub fn mk_struct_named(&mut self, name: String) -> TypeId {
        if let Some(&id) = self.named_struct_map.get(&name) {
            return id;
        }
        let id = TypeId(self.types.len() as u32);
        self.types.push(TypeData::Struct(StructType {
            name: Some(name.clone()),
            fields: Vec::new(),
            packed: false,
        }));
        self.named_struct_map.insert(name, id);
        id
    }

    /// Fill in the body of a previously-created named struct.
    pub fn define_struct_body(&mut self, id: TypeId, fields: Vec<TypeId>, packed: bool) {
        if let TypeData::Struct(st) = &mut self.types[id.0 as usize] {
            st.fields = fields;
            st.packed = packed;
        }
    }

    /// Look up a named struct by name.
    pub fn get_named_struct(&self, name: &str) -> Option<TypeId> {
        self.named_struct_map.get(name).copied()
    }

    // -----------------------------------------------------------------------
    // Type accessors
    // -----------------------------------------------------------------------

    pub fn get_type(&self, id: TypeId) -> &TypeData {
        &self.types[id.0 as usize]
    }

    pub fn get_type_mut(&mut self, id: TypeId) -> &mut TypeData {
        &mut self.types[id.0 as usize]
    }

    /// Total number of interned types.
    pub fn num_types(&self) -> usize {
        self.types.len()
    }

    /// Iterate over all (TypeId, TypeData) pairs.
    pub fn types(&self) -> impl Iterator<Item = (TypeId, &TypeData)> {
        self.types.iter().enumerate().map(|(i, td)| (TypeId(i as u32), td))
    }

    // -----------------------------------------------------------------------
    // Constant constructors
    // -----------------------------------------------------------------------

    pub fn const_int(&mut self, ty: TypeId, val: u64) -> ConstId {
        let key = ConstantKey::Int(ty, val);
        if let Some(&id) = self.const_map.get(&key) {
            return id;
        }
        let id = ConstId(self.constants.len() as u32);
        self.constants.push(ConstantData::Int { ty, val });
        self.const_map.insert(key, id);
        id
    }

    /// Store a float constant as raw bits (f32 bits in low 32 for Single,
    /// full u64 bits for Double / other).
    pub fn const_float(&mut self, ty: TypeId, bits: u64) -> ConstId {
        let key = ConstantKey::Float(ty, bits);
        if let Some(&id) = self.const_map.get(&key) {
            return id;
        }
        let id = ConstId(self.constants.len() as u32);
        self.constants.push(ConstantData::Float { ty, bits });
        self.const_map.insert(key, id);
        id
    }

    pub fn const_null(&mut self, ty: TypeId) -> ConstId {
        let key = ConstantKey::Null(ty);
        if let Some(&id) = self.const_map.get(&key) {
            return id;
        }
        let id = ConstId(self.constants.len() as u32);
        self.constants.push(ConstantData::Null(ty));
        self.const_map.insert(key, id);
        id
    }

    pub fn const_undef(&mut self, ty: TypeId) -> ConstId {
        let key = ConstantKey::Undef(ty);
        if let Some(&id) = self.const_map.get(&key) {
            return id;
        }
        let id = ConstId(self.constants.len() as u32);
        self.constants.push(ConstantData::Undef(ty));
        self.const_map.insert(key, id);
        id
    }

    pub fn const_poison(&mut self, ty: TypeId) -> ConstId {
        let key = ConstantKey::Poison(ty);
        if let Some(&id) = self.const_map.get(&key) {
            return id;
        }
        let id = ConstId(self.constants.len() as u32);
        self.constants.push(ConstantData::Poison(ty));
        self.const_map.insert(key, id);
        id
    }

    pub fn const_zero(&mut self, ty: TypeId) -> ConstId {
        let key = ConstantKey::ZeroInitializer(ty);
        if let Some(&id) = self.const_map.get(&key) {
            return id;
        }
        let id = ConstId(self.constants.len() as u32);
        self.constants.push(ConstantData::ZeroInitializer(ty));
        self.const_map.insert(key, id);
        id
    }

    /// Push a complex (non-scalar) constant without deduplication.
    pub fn push_const(&mut self, c: ConstantData) -> ConstId {
        let id = ConstId(self.constants.len() as u32);
        self.constants.push(c);
        id
    }

    // -----------------------------------------------------------------------
    // Constant accessors
    // -----------------------------------------------------------------------

    pub fn get_const(&self, id: ConstId) -> &ConstantData {
        &self.constants[id.0 as usize]
    }

    pub fn type_of_const(&self, id: ConstId) -> TypeId {
        match &self.constants[id.0 as usize] {
            ConstantData::Int { ty, .. } => *ty,
            ConstantData::IntWide { ty, .. } => *ty,
            ConstantData::Float { ty, .. } => *ty,
            ConstantData::Null(ty) => *ty,
            ConstantData::Undef(ty) => *ty,
            ConstantData::Poison(ty) => *ty,
            ConstantData::ZeroInitializer(ty) => *ty,
            ConstantData::Array { ty, .. } => *ty,
            ConstantData::Struct { ty, .. } => *ty,
            ConstantData::Vector { ty, .. } => *ty,
            ConstantData::GlobalRef { ty, .. } => *ty,
        }
    }
}

impl Default for Context {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn singleton_types() {
        let ctx = Context::new();
        // Verify pre-interned singletons are distinct
        assert_ne!(ctx.void_ty, ctx.i32_ty);
        assert_ne!(ctx.i32_ty, ctx.i64_ty);
        assert_ne!(ctx.f32_ty, ctx.f64_ty);
        assert_ne!(ctx.ptr_ty, ctx.i32_ty);
    }

    #[test]
    fn type_interning() {
        let mut ctx = Context::new();
        let a = ctx.mk_int(32);
        let b = ctx.mk_int(32);
        assert_eq!(a, b);
        let c = ctx.mk_int(64);
        assert_ne!(a, c);
        assert_eq!(a, ctx.i32_ty);
        assert_eq!(c, ctx.i64_ty);
    }

    #[test]
    fn named_struct() {
        let mut ctx = Context::new();
        let id1 = ctx.mk_struct_named("Foo".to_string());
        let id2 = ctx.mk_struct_named("Foo".to_string());
        assert_eq!(id1, id2);
        let id3 = ctx.mk_struct_named("Bar".to_string());
        assert_ne!(id1, id3);
    }

    #[test]
    fn const_int_dedup() {
        let mut ctx = Context::new();
        let c1 = ctx.const_int(ctx.i32_ty, 42);
        let c2 = ctx.const_int(ctx.i32_ty, 42);
        assert_eq!(c1, c2);
        let c3 = ctx.const_int(ctx.i32_ty, 0);
        assert_ne!(c1, c3);
    }
}
