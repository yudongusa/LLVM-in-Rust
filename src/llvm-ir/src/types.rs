//! IR type system: integer, float, pointer, vector, struct, array, and function types.

use crate::context::TypeId;

/// Public API for `TypeData`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum TypeData {
    /// `Void` variant.
    Void,
    /// `Integer` variant.
    Integer(u32),
    /// `Float` variant.
    Float(FloatKind),
    /// `Pointer` variant.
    Pointer,
    /// `Array` variant.
    Array {
        element: TypeId,
        len: u64,
    },
    /// `Vector` variant.
    Vector {
        element: TypeId,
        len: u32,
        scalable: bool,
    },
    /// `Struct` variant.
    Struct(StructType),
    /// `Function` variant.
    Function(FunctionType),
    /// `Label` variant.
    Label,
    /// `Metadata` variant.
    Metadata,
}

/// Public API for `FloatKind`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FloatKind {
    /// `Half` variant.
    Half,
    /// `BFloat` variant.
    BFloat,
    /// `Single` variant.
    Single,
    /// `Double` variant.
    Double,
    /// `Fp128` variant.
    Fp128,
    /// `X86Fp80` variant.
    X86Fp80,
}

/// Public API for `StructType`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct StructType {
    /// Public API for `name`.
    pub name: Option<String>,
    /// Public API for `fields`.
    pub fields: Vec<TypeId>,
    /// Public API for `packed`.
    pub packed: bool,
}

/// Public API for `FunctionType`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FunctionType {
    /// Public API for `ret`.
    pub ret: TypeId,
    /// Public API for `params`.
    pub params: Vec<TypeId>,
    /// Public API for `variadic`.
    pub variadic: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::Context;

    #[test]
    fn type_data_eq() {
        let a = TypeData::Integer(32);
        let b = TypeData::Integer(32);
        assert_eq!(a, b);
        let c = TypeData::Integer(64);
        assert_ne!(a, c);
    }

    #[test]
    fn struct_type_eq() {
        let ctx = Context::new();
        let s1 = StructType {
            name: None,
            fields: vec![ctx.i32_ty, ctx.i64_ty],
            packed: false,
        };
        let s2 = StructType {
            name: None,
            fields: vec![ctx.i32_ty, ctx.i64_ty],
            packed: false,
        };
        assert_eq!(s1, s2);
    }
}
