//! WebAssembly value-type and function-type encoding helpers.

use llvm_ir::{Context, FloatKind, TypeData, TypeId};

// ── wasm value type bytes (valtype) ───────────────────────────────────────

/// `i32` valtype.
pub const VALTYPE_I32: u8 = 0x7F;
/// `i64` valtype.
pub const VALTYPE_I64: u8 = 0x7E;
/// `f32` valtype.
pub const VALTYPE_F32: u8 = 0x7D;
/// `f64` valtype.
pub const VALTYPE_F64: u8 = 0x7C;

/// Wasm block type: empty (void result).
pub const BLOCKTYPE_VOID: u8 = 0x40;

/// Map an IR `TypeId` to the closest wasm `valtype` byte.
///
/// - i1 / i8 / i16 / i32 → `i32`
/// - i64 / pointer        → `i64`
/// - f32                  → `f32`
/// - f64                  → `f64`
/// - everything else      → `i32` (conservative default)
pub fn ir_ty_to_valtype(ctx: &Context, ty: TypeId) -> Option<u8> {
    match ctx.get_type(ty) {
        TypeData::Void => None,
        TypeData::Integer(bits) => {
            if *bits <= 32 {
                Some(VALTYPE_I32)
            } else {
                Some(VALTYPE_I64)
            }
        }
        TypeData::Float(FloatKind::Single) => Some(VALTYPE_F32),
        TypeData::Float(FloatKind::Double) => Some(VALTYPE_F64),
        TypeData::Float(_) => Some(VALTYPE_F64),
        // Pointers are represented as i32 in wasm32
        TypeData::Pointer => Some(VALTYPE_I32),
        // Fallback for exotic types
        _ => Some(VALTYPE_I32),
    }
}

/// Return `true` if this IR type maps to a 64-bit wasm integer.
pub fn is_i64_type(ctx: &Context, ty: TypeId) -> bool {
    matches!(ctx.get_type(ty), TypeData::Integer(bits) if *bits > 32)
}

