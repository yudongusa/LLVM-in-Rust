//! SSA values: constants, instruction results, function arguments, and globals.

use crate::context::{ConstId, GlobalId, TypeId};

/// Constant value stored in the Context constant pool.
#[allow(missing_docs)]
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
    GlobalRef {
        ty: TypeId,
        id: GlobalId,
        name: String,
    },
    /// LLVM constant expression — e.g. `getelementptr (T, ptr @gv, i64 3)`
    /// inside a global initializer that can't be hoisted to an SSA
    /// instruction.  Issue #207 PR-B.
    Expr {
        /// Result type of the expression.
        ty: TypeId,
        /// Public API for `op`.
        op: ConstExprOp,
        /// Constant operands.  For `GetElementPtr`, operands[0] is the base
        /// pointer; operands[1..] are the indices.  For unary casts,
        /// operands[0] is the source value.
        operands: Vec<ConstId>,
    },
}

/// Operation tag for [`ConstantData::Expr`].  Only the constexpr forms
/// emitted by clang at `-O0` are represented; richer ops (`add`, `select`,
/// `icmp`, etc.) are a follow-up if compatibility surveys turn them up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConstExprOp {
    /// `getelementptr [inbounds] (T, ptr <base>, indices...)`
    GetElementPtr {
        /// Whether the constexpr GEP carries the `inbounds` flag.
        inbounds: bool,
        /// Aggregate / source-element type for index arithmetic.
        base_ty: TypeId,
    },
    /// `bitcast (T C to U)`
    BitCast,
    /// `inttoptr (iN C to ptr)`
    IntToPtr,
    /// `ptrtoint (ptr C to iN)`
    PtrToInt,
    /// `addrspacecast (T C to U)`
    AddrSpaceCast,
    /// `trunc (T C to U)`
    Trunc,
    /// `zext (T C to U)`
    ZExt,
    /// `sext (T C to U)`
    SExt,
}

impl ConstExprOp {
    /// Textual opcode spelling (no parentheses / operands).
    pub fn as_str(self) -> &'static str {
        match self {
            ConstExprOp::GetElementPtr { .. } => "getelementptr",
            ConstExprOp::BitCast => "bitcast",
            ConstExprOp::IntToPtr => "inttoptr",
            ConstExprOp::PtrToInt => "ptrtoint",
            ConstExprOp::AddrSpaceCast => "addrspacecast",
            ConstExprOp::Trunc => "trunc",
            ConstExprOp::ZExt => "zext",
            ConstExprOp::SExt => "sext",
        }
    }

    /// `true` for cast-shaped ops (single operand, written `<op> (T C to U)`).
    pub fn is_cast(self) -> bool {
        matches!(
            self,
            ConstExprOp::BitCast
                | ConstExprOp::IntToPtr
                | ConstExprOp::PtrToInt
                | ConstExprOp::AddrSpaceCast
                | ConstExprOp::Trunc
                | ConstExprOp::ZExt
                | ConstExprOp::SExt
        )
    }
}

/// A function argument (SSA value produced by function entry).
#[derive(Clone, Debug)]
pub struct Argument {
    /// Public API for `name`.
    pub name: String,
    /// Public API for `ty`.
    pub ty: TypeId,
    /// Public API for `index`.
    pub index: u32,
}

/// A global variable definition.
#[derive(Clone, Debug)]
pub struct GlobalVariable {
    /// Public API for `name`.
    pub name: String,
    /// Type of the value stored (not the pointer type).
    pub ty: TypeId,
    /// Optional constant initializer.
    pub initializer: Option<ConstId>,
    /// If true, the global is read-only.
    pub is_constant: bool,
    /// Public API for `linkage`.
    pub linkage: Linkage,
}

/// Linkage kinds matching LLVM IR semantics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Linkage {
    /// `Private` variant.
    Private,
    /// `Internal` variant.
    Internal,
    /// `External` variant.
    #[default]
    External,
    /// `Weak` variant.
    Weak,
    /// `WeakOdr` variant.
    WeakOdr,
    /// `LinkOnce` variant.
    LinkOnce,
    /// `LinkOnceOdr` variant.
    LinkOnceOdr,
    /// `Common` variant.
    Common,
    /// `AvailableExternally` variant.
    AvailableExternally,
}

impl Linkage {
    /// Public API for `as_str`.
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

    /// Public API for `is_external`.
    pub fn is_external(self) -> bool {
        self == Linkage::External
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
