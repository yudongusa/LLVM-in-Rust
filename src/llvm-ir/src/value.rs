//! SSA values: constants, instruction results, function arguments, and globals.

use crate::context::{TypeId, ConstId, GlobalId};

/// Constant value stored in the Context constant pool.
#[derive(Clone, Debug, PartialEq)]
pub enum ConstantData {
    /// Small integer (fits in u64).
    Int { ty: TypeId, val: u64 },
    /// Wide integer (more than 64 bits), stored as little-endian 64-bit words.
    IntWide { ty: TypeId, words: Vec<u64> },
    /// Floating-point value stored as raw bits.
    Float { ty: TypeId, bits: u64 },
    /// Null / null pointer / zero pointer.
    Null(TypeId),
    /// Undef value.
    Undef(TypeId),
    /// Poison value.
    Poison(TypeId),
    /// Zero-initializer (aggregate types).
    ZeroInitializer(TypeId),
    /// Constant array.
    Array { ty: TypeId, elements: Vec<ConstId> },
    /// Constant struct.
    Struct { ty: TypeId, fields: Vec<ConstId> },
    /// Constant vector.
    Vector { ty: TypeId, elements: Vec<ConstId> },
    /// Reference to a global symbol (global variable or function).
    /// `name` is the LLVM IR name (without `@`), used for printing.
    GlobalRef { ty: TypeId, id: GlobalId, name: String },
}

/// A function argument (SSA value produced by function entry).
#[derive(Clone, Debug)]
pub struct Argument {
    pub name: String,
    pub ty: TypeId,
    pub index: u32,
}

/// A global variable definition.
#[derive(Clone, Debug)]
pub struct GlobalVariable {
    pub name: String,
    /// Type of the value stored (not the pointer type).
    pub ty: TypeId,
    /// Optional constant initializer.
    pub initializer: Option<ConstId>,
    /// If true, the global is read-only.
    pub is_constant: bool,
    pub linkage: Linkage,
}

/// Linkage kinds matching LLVM IR semantics.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Linkage {
    Private,
    Internal,
    External,
    Weak,
    WeakOdr,
    LinkOnce,
    LinkOnceOdr,
    Common,
    AvailableExternally,
}

impl Linkage {
    pub fn as_str(self) -> &'static str {
        match self {
            Linkage::Private => "private",
            Linkage::Internal => "internal",
            Linkage::External => "",
            Linkage::Weak => "weak",
            Linkage::WeakOdr => "weak_odr",
            Linkage::LinkOnce => "linkonce",
            Linkage::LinkOnceOdr => "linkonce_odr",
            Linkage::Common => "common",
            Linkage::AvailableExternally => "available_externally",
        }
    }

    pub fn is_external(self) -> bool {
        self == Linkage::External
    }
}

impl Default for Linkage {
    fn default() -> Self {
        Linkage::External
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linkage_str() {
        assert_eq!(Linkage::Private.as_str(), "private");
        assert_eq!(Linkage::External.as_str(), "");
        assert_eq!(Linkage::Internal.as_str(), "internal");
    }
}
