//! x86_64 calling-convention support (System V AMD64 and Windows x64).

use crate::regs::{
    FP_ALLOCATABLE, R10, R11, R12, R13, R14, R15, R8, R9, RAX, RBP, RBX, RCX, RDI, RDX, RSI,
    XMM0, XMM1, XMM2, XMM3,
};
use llvm_codegen::isel::PReg;
use llvm_ir::{Context, FloatKind, TypeData, TypeId};

// ── System V AMD64 ABI ─────────────────────────────────────────────────────

/// Integer/pointer argument registers in System V AMD64 order.
pub const SYSV_INT_ARGS: &[PReg] = &[RDI, RSI, RDX, RCX, R8, R9];

/// Floating-point argument registers in System V AMD64 order (XMM0–XMM7).
pub const SYSV_FP_ARGS: &[PReg] = FP_ALLOCATABLE;

/// Integer return register (System V AMD64).
pub const SYSV_INT_RET: PReg = RAX;

/// Floating-point return register (System V AMD64).
pub const SYSV_FP_RET: PReg = XMM0;

/// Caller-saved allocatable GPRs for System V AMD64.
pub const SYSV_CALLER_SAVED: &[PReg] = &[RAX, RCX, RDX, RSI, RDI, R8, R9, R10, R11];

/// Allocatable GPRs for System V AMD64.
pub const SYSV_ALLOCATABLE: &[PReg] = SYSV_CALLER_SAVED;

/// Callee-saved GPRs for System V AMD64.
pub const SYSV_CALLEE_SAVED: &[PReg] = &[RBX, RBP, R12, R13, R14, R15];

// ── Windows x64 ABI ───────────────────────────────────────────────────────

/// Integer/pointer argument registers in Windows x64 order.
pub const WIN64_INT_ARGS: &[PReg] = &[RCX, RDX, R8, R9];

/// Floating-point argument registers in Windows x64 order (XMM0–XMM3).
/// Win64 shares the slot index with int args (arg 0 is XMM0 OR RCX, not both).
pub const WIN64_FP_ARGS: &[PReg] = &[XMM0, XMM1, XMM2, XMM3];

/// Integer return register (Windows x64).
pub const WIN64_INT_RET: PReg = RAX;

/// Floating-point return register (Windows x64).
pub const WIN64_FP_RET: PReg = XMM0;

/// Caller-saved GPRs for Windows x64.
pub const WIN64_CALLER_SAVED: &[PReg] = &[RAX, RCX, RDX, R8, R9, R10, R11];

/// Allocatable GPRs for Windows x64.
///
/// Include RSI/RDI even though they are callee-saved under Win64. The register
/// allocator records any callee-saved assignments in `used_callee_saved`, and
/// the x86 emitter preserves those registers in the prologue/epilogue. Keeping
/// them allocatable gives Win64 the same low-pressure headroom as the SysV smoke
/// path while still preserving ABI clobber semantics for calls.
pub const WIN64_ALLOCATABLE: &[PReg] = &[RAX, RCX, RDX, RSI, RDI, R8, R9, R10, R11];

/// Callee-saved GPRs for Windows x64.
pub const WIN64_CALLEE_SAVED: &[PReg] = &[RBX, RBP, RSI, RDI, R12, R13, R14, R15];

// ── argument location ──────────────────────────────────────────────────────

/// Where a single argument lands after ABI classification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArgLocation {
    /// Passed in the given integer/pointer register.
    Reg(PReg),
    /// Passed in the given XMM (floating-point) register.
    FpReg(PReg),
    /// Passed on the stack at `offset` bytes above RSP at the call site
    /// (first stack argument is at offset 0).
    Stack(u32),
    /// Passed by pointer: caller allocates a copy and passes its address in the
    /// next available integer register slot (SysV AMD64 MEMORY class).
    ByPtr,
}

// ── SysV struct classification ─────────────────────────────────────────────

/// Classification of a single 8-byte "eightbyte" chunk of a struct argument
/// under the SysV AMD64 ABI (§3.2.3 of the ABI spec).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EightbyteClass {
    /// Contains at least one integer or pointer field (wins over SSE).
    Integer,
    /// Contains only floating-point fields (float or double).
    Sse,
    /// Struct is too large (>16 bytes), unaligned, or contains an
    /// unsupported type; must be passed in memory (by pointer).
    Memory,
}

/// Return the byte size of a type for SysV layout purposes.
///
/// Returns `None` for types that have no defined size (e.g. void, label) or
/// that force MEMORY classification (i128, vectors, etc.).
fn type_size(ctx: &Context, ty: TypeId) -> Option<u64> {
    match ctx.get_type(ty) {
        TypeData::Integer(bits) => Some((*bits as u64 + 7) / 8),
        TypeData::Float(FloatKind::Single) => Some(4),
        TypeData::Float(FloatKind::Double) => Some(8),
        TypeData::Float(_) => None, // x87 / fp128 / bfloat → MEMORY
        TypeData::Pointer => Some(8),
        TypeData::Struct(st) => {
            let mut total: u64 = 0;
            for &f in &st.fields.clone() {
                total += type_size(ctx, f)?;
            }
            Some(total)
        }
        _ => None,
    }
}

/// Classify a single field type for eightbyte merging.
///
/// Returns `Some(EightbyteClass)` for field types that are natively
/// classifiable, or `None` to signal MEMORY (unsupported type, etc.).
fn classify_field(ctx: &Context, ty: TypeId) -> EightbyteClass {
    match ctx.get_type(ty) {
        TypeData::Integer(_) | TypeData::Pointer => EightbyteClass::Integer,
        TypeData::Float(FloatKind::Single) | TypeData::Float(FloatKind::Double) => {
            EightbyteClass::Sse
        }
        TypeData::Float(_) => EightbyteClass::Memory,
        TypeData::Struct(_) => {
            // Recursively: if any sub-field forces MEMORY the whole thing does.
            let sub = classify_struct_sysv(ctx, ty);
            if sub == [EightbyteClass::Memory] {
                EightbyteClass::Memory
            } else {
                // Nested struct: return INTEGER if any eightbyte is INTEGER.
                if sub.iter().any(|c| *c == EightbyteClass::Integer) {
                    EightbyteClass::Integer
                } else {
                    EightbyteClass::Sse
                }
            }
        }
        _ => EightbyteClass::Memory,
    }
}

/// Merge two eightbyte classifications following SysV §3.2.3 merge rules.
fn merge(a: EightbyteClass, b: EightbyteClass) -> EightbyteClass {
    use EightbyteClass::*;
    match (a, b) {
        (Memory, _) | (_, Memory) => Memory,
        (Integer, _) | (_, Integer) => Integer,
        _ => Sse,
    }
}

/// Classify a struct type for SysV AMD64 ABI argument passing.
///
/// Returns one [`EightbyteClass`] per 8-byte chunk of the struct, or
/// `vec![EightbyteClass::Memory]` when the struct must be passed by pointer.
///
/// Rules implemented:
/// - Total size > 16 bytes → MEMORY (all other cases return 1 or 2 classes).
/// - Each 8-byte chunk is classified based on the field types it contains:
///   INTEGER wins over SSE; any MEMORY field makes the chunk MEMORY.
pub fn classify_struct_sysv(ctx: &Context, ty: TypeId) -> Vec<EightbyteClass> {
    let memory = vec![EightbyteClass::Memory];

    let st = match ctx.get_type(ty) {
        TypeData::Struct(st) => st.clone(),
        _ => return memory,
    };

    // Total size check.
    let total = match type_size(ctx, ty) {
        Some(n) => n,
        None => return memory,
    };
    if total > 16 {
        return memory;
    }

    // Walk fields and accumulate their contribution to eightbytes.
    // We track `byte_offset` to determine which eightbyte a field lands in.
    let n_eightbytes = if total <= 8 { 1 } else { 2 };
    let mut classes: Vec<EightbyteClass> = vec![EightbyteClass::Sse; n_eightbytes];
    // Start with SSE (neutral) — INTEGER and MEMORY override.

    let mut byte_offset: u64 = 0;
    for &field_ty in &st.fields {
        let field_size = match type_size(ctx, field_ty) {
            Some(s) => s,
            None => return memory,
        };
        let eb_idx = (byte_offset / 8) as usize;
        let field_class = classify_field(ctx, field_ty);
        if field_class == EightbyteClass::Memory {
            return memory;
        }
        classes[eb_idx] = merge(classes[eb_idx].clone(), field_class);
        byte_offset += field_size;
    }

    classes
}

/// Active x86-64 calling convention.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallingConvention {
    /// `SysV` variant.
    SysV,
    /// `Win64` variant.
    Win64,
}

impl CallingConvention {
    /// Infer ABI from target triple (or host OS if triple is absent).
    pub fn from_target_triple(triple: Option<&str>) -> Self {
        if let Some(t) = triple {
            let t = t.to_ascii_lowercase();
            if t.contains("windows") || t.contains("win32") || t.contains("mingw") {
                return Self::Win64;
            }
        }
        if cfg!(target_os = "windows") {
            Self::Win64
        } else {
            Self::SysV
        }
    }

    /// Classify arguments treating all as integer (existing callers, no type info).
    pub fn classify_int_args(self, n_args: usize) -> Vec<ArgLocation> {
        match self {
            Self::SysV => classify_sysv_args(n_args),
            Self::Win64 => classify_win64_args(n_args),
        }
    }

    /// Classify arguments with type information: `is_fp[i]` is true for float/double args.
    pub fn classify_args_typed(self, is_fp: &[bool]) -> Vec<ArgLocation> {
        match self {
            Self::SysV => classify_sysv_args_typed(is_fp),
            Self::Win64 => classify_win64_args_typed(is_fp),
        }
    }

    /// Public API for `int_ret`.
    pub fn int_ret(self) -> PReg {
        match self {
            Self::SysV => SYSV_INT_RET,
            Self::Win64 => WIN64_INT_RET,
        }
    }

    /// FP return register (XMM0 on both ABIs).
    pub fn fp_ret(self) -> PReg {
        match self {
            Self::SysV => SYSV_FP_RET,
            Self::Win64 => WIN64_FP_RET,
        }
    }

    /// Public API for `allocatable_pregs`.
    pub fn allocatable_pregs(self) -> &'static [PReg] {
        match self {
            Self::SysV => SYSV_ALLOCATABLE,
            Self::Win64 => WIN64_ALLOCATABLE,
        }
    }

    /// Public API for `callee_saved_pregs`.
    pub fn callee_saved_pregs(self) -> &'static [PReg] {
        match self {
            Self::SysV => SYSV_CALLEE_SAVED,
            Self::Win64 => WIN64_CALLEE_SAVED,
        }
    }

    /// Public API for `caller_saved_clobbers`.
    pub fn caller_saved_clobbers(self) -> &'static [PReg] {
        match self {
            Self::SysV => SYSV_CALLER_SAVED,
            Self::Win64 => WIN64_CALLER_SAVED,
        }
    }

    /// Public API for `shadow_space_bytes`.
    pub fn shadow_space_bytes(self) -> u32 {
        match self {
            Self::SysV => 0,
            Self::Win64 => 32,
        }
    }
}

/// Classify `n_args` integer/pointer arguments using the System V AMD64 ABI.
///
/// Arguments 0–5 go into registers; arguments 6+ go on the stack.
pub fn classify_sysv_args(n_args: usize) -> Vec<ArgLocation> {
    (0..n_args)
        .map(|i| {
            if i < SYSV_INT_ARGS.len() {
                ArgLocation::Reg(SYSV_INT_ARGS[i])
            } else {
                ArgLocation::Stack(((i - SYSV_INT_ARGS.len()) * 8) as u32)
            }
        })
        .collect()
}

/// Classify arguments using System V AMD64 ABI with type information.
///
/// Integer/pointer args consume integer register slots (RDI, RSI, …); float/double
/// args consume XMM register slots (XMM0, XMM1, …) independently.  Either pool
/// exhausted → stack.
pub fn classify_sysv_args_typed(is_fp: &[bool]) -> Vec<ArgLocation> {
    let mut int_idx = 0usize;
    let mut fp_idx = 0usize;
    let mut stack_off = 0u32;

    is_fp
        .iter()
        .map(|&fp| {
            if fp {
                if fp_idx < SYSV_FP_ARGS.len() {
                    let r = SYSV_FP_ARGS[fp_idx];
                    fp_idx += 1;
                    ArgLocation::FpReg(r)
                } else {
                    let off = stack_off;
                    stack_off += 8;
                    ArgLocation::Stack(off)
                }
            } else {
                if int_idx < SYSV_INT_ARGS.len() {
                    let r = SYSV_INT_ARGS[int_idx];
                    int_idx += 1;
                    ArgLocation::Reg(r)
                } else {
                    let off = stack_off;
                    stack_off += 8;
                    ArgLocation::Stack(off)
                }
            }
        })
        .collect()
}

/// Classify `n_args` integer/pointer arguments using the Windows x64 ABI.
///
/// Arguments 0–3 go into registers; arguments 4+ go on the stack.
pub fn classify_win64_args(n_args: usize) -> Vec<ArgLocation> {
    (0..n_args)
        .map(|i| {
            if i < WIN64_INT_ARGS.len() {
                ArgLocation::Reg(WIN64_INT_ARGS[i])
            } else {
                ArgLocation::Stack(((i - WIN64_INT_ARGS.len()) * 8) as u32)
            }
        })
        .collect()
}

/// Classify arguments using Windows x64 ABI with type information.
///
/// Win64 uses a unified slot index: argument N goes in XMM_N (if FP) or INT_N (if int),
/// where N is the argument position (0-3).  Slots 4+ go on the stack.
pub fn classify_win64_args_typed(is_fp: &[bool]) -> Vec<ArgLocation> {
    is_fp
        .iter()
        .enumerate()
        .map(|(i, &fp)| {
            if i < 4 {
                if fp {
                    ArgLocation::FpReg(WIN64_FP_ARGS[i])
                } else {
                    ArgLocation::Reg(WIN64_INT_ARGS[i])
                }
            } else {
                ArgLocation::Stack(((i - 4) * 8) as u32)
            }
        })
        .collect()
}

// ── tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::regs::{R10, R11, R8, R9, RAX, RCX, RDI, RDX, RSI, XMM0, XMM1, XMM7};
    use llvm_ir::Context;

    #[test]
    fn sysv_first_six_in_registers() {
        let locs = classify_sysv_args(6);
        assert_eq!(locs[0], ArgLocation::Reg(RDI));
        assert_eq!(locs[1], ArgLocation::Reg(RSI));
        assert_eq!(locs[2], ArgLocation::Reg(RDX));
        assert_eq!(locs[3], ArgLocation::Reg(RCX));
        assert_eq!(locs[4], ArgLocation::Reg(R8));
        assert_eq!(locs[5], ArgLocation::Reg(R9));
    }

    #[test]
    fn sysv_seventh_on_stack() {
        let locs = classify_sysv_args(7);
        assert_eq!(locs[6], ArgLocation::Stack(0));
    }

    #[test]
    fn sysv_eighth_on_stack_offset_8() {
        let locs = classify_sysv_args(8);
        assert_eq!(locs[7], ArgLocation::Stack(8));
    }

    #[test]
    fn win64_first_four_in_registers() {
        let locs = classify_win64_args(4);
        assert_eq!(locs[0], ArgLocation::Reg(RCX));
        assert_eq!(locs[1], ArgLocation::Reg(RDX));
        assert_eq!(locs[2], ArgLocation::Reg(R8));
        assert_eq!(locs[3], ArgLocation::Reg(R9));
    }

    #[test]
    fn win64_fifth_on_stack() {
        let locs = classify_win64_args(5);
        assert_eq!(locs[4], ArgLocation::Stack(0));
    }

    #[test]
    fn zero_args_empty() {
        assert!(classify_sysv_args(0).is_empty());
        assert!(classify_win64_args(0).is_empty());
    }

    #[test]
    fn infer_from_windows_triple() {
        let cc = CallingConvention::from_target_triple(Some("x86_64-pc-windows-msvc"));
        assert_eq!(cc, CallingConvention::Win64);
    }

    #[test]
    fn win64_allocates_rsi_rdi_but_does_not_mark_them_call_clobbered() {
        assert!(WIN64_CALLEE_SAVED.contains(&RSI));
        assert!(WIN64_CALLEE_SAVED.contains(&RDI));
        assert!(WIN64_ALLOCATABLE.contains(&RSI));
        assert!(WIN64_ALLOCATABLE.contains(&RDI));

        let clobbers = CallingConvention::Win64.caller_saved_clobbers();
        assert_eq!(clobbers, &[RAX, RCX, RDX, R8, R9, R10, R11]);
        assert!(!clobbers.contains(&RSI));
        assert!(!clobbers.contains(&RDI));
    }

    #[test]
    fn sysv_typed_fp_args_go_in_xmm() {
        // (f64, f64) → XMM0, XMM1
        let locs = classify_sysv_args_typed(&[true, true]);
        assert_eq!(locs[0], ArgLocation::FpReg(XMM0));
        assert_eq!(locs[1], ArgLocation::FpReg(XMM1));
    }

    #[test]
    fn sysv_typed_mixed_int_fp_args() {
        // (i64, f64, i64, f64) → RDI, XMM0, RSI, XMM1
        let locs = classify_sysv_args_typed(&[false, true, false, true]);
        assert_eq!(locs[0], ArgLocation::Reg(RDI));
        assert_eq!(locs[1], ArgLocation::FpReg(XMM0));
        assert_eq!(locs[2], ArgLocation::Reg(RSI));
        assert_eq!(locs[3], ArgLocation::FpReg(XMM1));
    }

    #[test]
    fn sysv_typed_fp_overflow_goes_to_stack() {
        // 9 f64 args: first 8 → XMM0-XMM7, 9th → stack
        let is_fp = vec![true; 9];
        let locs = classify_sysv_args_typed(&is_fp);
        assert_eq!(locs[7], ArgLocation::FpReg(XMM7));
        assert_eq!(locs[8], ArgLocation::Stack(0));
    }

    #[test]
    fn win64_typed_fp_args_go_in_xmm_by_slot() {
        // (f64, f64) → XMM0, XMM1 (slot-based)
        let locs = classify_win64_args_typed(&[true, true]);
        assert_eq!(locs[0], ArgLocation::FpReg(XMM0));
        assert_eq!(locs[1], ArgLocation::FpReg(XMM1));
    }

    #[test]
    fn win64_typed_mixed_int_fp_by_slot() {
        // (i64, f64, i64) → slot 0=RCX, slot 1=XMM1, slot 2=R8
        let locs = classify_win64_args_typed(&[false, true, false]);
        assert_eq!(locs[0], ArgLocation::Reg(RCX));
        assert_eq!(locs[1], ArgLocation::FpReg(XMM1));
        assert_eq!(locs[2], ArgLocation::Reg(R8));
    }

    // ── SysV struct classification tests ──────────────────────────────────────

    /// `{i32, i32}` — 4+4 = 8 bytes, one eightbyte, both integer → [Integer]
    #[test]
    fn classify_i32_i32_struct_is_integer() {
        let mut ctx = Context::new();
        let i32_ty = ctx.i32_ty;
        let ty = ctx.mk_struct_anon(vec![i32_ty, i32_ty], false);
        let cls = classify_struct_sysv(&ctx, ty);
        assert_eq!(cls, vec![EightbyteClass::Integer]);
    }

    /// `{double, double}` — 8+8 = 16 bytes, two eightbytes, both SSE → [Sse, Sse]
    #[test]
    fn classify_double_double_struct_is_sse_sse() {
        let mut ctx = Context::new();
        let f64_ty = ctx.f64_ty;
        let ty = ctx.mk_struct_anon(vec![f64_ty, f64_ty], false);
        let cls = classify_struct_sysv(&ctx, ty);
        assert_eq!(cls, vec![EightbyteClass::Sse, EightbyteClass::Sse]);
    }

    /// `{i64, i64, i64}` — 24 bytes > 16 → [Memory]
    #[test]
    fn classify_large_struct_is_memory() {
        let mut ctx = Context::new();
        let i64_ty = ctx.i64_ty;
        let ty = ctx.mk_struct_anon(vec![i64_ty, i64_ty, i64_ty], false);
        let cls = classify_struct_sysv(&ctx, ty);
        assert_eq!(cls, vec![EightbyteClass::Memory]);
    }

    /// `{ptr, i64}` — pointer + integer, two eightbytes → [Integer, Integer]
    #[test]
    fn classify_pointer_struct_is_integer_integer() {
        let mut ctx = Context::new();
        let ptr_ty = ctx.ptr_ty;
        let i64_ty = ctx.i64_ty;
        let ty = ctx.mk_struct_anon(vec![ptr_ty, i64_ty], false);
        let cls = classify_struct_sysv(&ctx, ty);
        assert_eq!(cls, vec![EightbyteClass::Integer, EightbyteClass::Integer]);
    }

    /// `{i32, float}` — 4+4 = 8 bytes, one eightbyte with both int and float.
    /// INTEGER wins over SSE → [Integer]
    #[test]
    fn classify_mixed_int_float_first_eightbyte_is_integer() {
        let mut ctx = Context::new();
        let i32_ty = ctx.i32_ty;
        let f32_ty = ctx.f32_ty;
        let ty = ctx.mk_struct_anon(vec![i32_ty, f32_ty], false);
        let cls = classify_struct_sysv(&ctx, ty);
        assert_eq!(cls, vec![EightbyteClass::Integer]);
    }

    /// `{i64, i64}` — two integer eightbytes → [Integer, Integer]
    #[test]
    fn classify_two_i64_is_integer_integer() {
        let mut ctx = Context::new();
        let i64_ty = ctx.i64_ty;
        let ty = ctx.mk_struct_anon(vec![i64_ty, i64_ty], false);
        let cls = classify_struct_sysv(&ctx, ty);
        assert_eq!(cls, vec![EightbyteClass::Integer, EightbyteClass::Integer]);
    }

    /// `{float}` — single float field, 4 bytes, one eightbyte → [Sse]
    #[test]
    fn classify_single_float_struct_is_sse() {
        let mut ctx = Context::new();
        let f32_ty = ctx.f32_ty;
        let ty = ctx.mk_struct_anon(vec![f32_ty], false);
        let cls = classify_struct_sysv(&ctx, ty);
        assert_eq!(cls, vec![EightbyteClass::Sse]);
    }
}
