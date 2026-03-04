//! IR type system: integer, float, pointer, vector, struct, array, and function types.

use crate::context::TypeId;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum TypeData {
    Void,
    Integer(u32),
    Float(FloatKind),
    Pointer,
    Array {
        element: TypeId,
        len: u64,
    },
    Vector {
        element: TypeId,
        len: u32,
        scalable: bool,
    },
    Struct(StructType),
    Function(FunctionType),
    Label,
    Metadata,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FloatKind {
    Half,
    BFloat,
    Single,
    Double,
    Fp128,
    X86Fp80,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct StructType {
    pub name: Option<String>,
    pub fields: Vec<TypeId>,
    pub packed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FunctionType {
    pub ret: TypeId,
    pub params: Vec<TypeId>,
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
