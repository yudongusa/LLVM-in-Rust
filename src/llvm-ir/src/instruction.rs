//! SSA instructions: arithmetic, memory, control flow, and call instructions.

use crate::context::{BlockId, TypeId, ValueRef};

// ---------------------------------------------------------------------------
// Flags
// ---------------------------------------------------------------------------

/// Flags for integer arithmetic instructions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IntArithFlags {
    /// No unsigned wrap.
    pub nuw: bool,
    /// No signed wrap.
    pub nsw: bool,
}

/// `exact` flag for division/shift instructions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExactFlag {
    pub exact: bool,
}

/// Fast-math flags for floating-point instructions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FastMathFlags {
    pub nnan: bool,
    pub ninf: bool,
    pub nsz: bool,
    pub arcp: bool,
    pub contract: bool,
    pub afn: bool,
    pub reassoc: bool,
    /// Shorthand for all flags set.
    pub fast: bool,
}

// ---------------------------------------------------------------------------
// Predicates
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IntPredicate {
    Eq,
    Ne,
    Ugt,
    Uge,
    Ult,
    Ule,
    Sgt,
    Sge,
    Slt,
    Sle,
}

impl IntPredicate {
    pub fn as_str(self) -> &'static str {
        match self {
            IntPredicate::Eq => "eq",
            IntPredicate::Ne => "ne",
            IntPredicate::Ugt => "ugt",
            IntPredicate::Uge => "uge",
            IntPredicate::Ult => "ult",
            IntPredicate::Ule => "ule",
            IntPredicate::Sgt => "sgt",
            IntPredicate::Sge => "sge",
            IntPredicate::Slt => "slt",
            IntPredicate::Sle => "sle",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FloatPredicate {
    False,
    Oeq,
    Ogt,
    Oge,
    Olt,
    Ole,
    One,
    Ord,
    Uno,
    Ueq,
    Ugt,
    Uge,
    Ult,
    Ule,
    Une,
    True,
}

impl FloatPredicate {
    pub fn as_str(self) -> &'static str {
        match self {
            FloatPredicate::False => "false",
            FloatPredicate::Oeq => "oeq",
            FloatPredicate::Ogt => "ogt",
            FloatPredicate::Oge => "oge",
            FloatPredicate::Olt => "olt",
            FloatPredicate::Ole => "ole",
            FloatPredicate::One => "one",
            FloatPredicate::Ord => "ord",
            FloatPredicate::Uno => "uno",
            FloatPredicate::Ueq => "ueq",
            FloatPredicate::Ugt => "ugt",
            FloatPredicate::Uge => "uge",
            FloatPredicate::Ult => "ult",
            FloatPredicate::Ule => "ule",
            FloatPredicate::Une => "une",
            FloatPredicate::True => "true",
        }
    }
}

/// Tail call optimization hint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TailCallKind {
    None,
    Tail,
    MustTail,
    NoTail,
}

// ---------------------------------------------------------------------------
// Instruction kind
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub enum InstrKind {
    // --- Integer arithmetic ---
    Add {
        flags: IntArithFlags,
        lhs: ValueRef,
        rhs: ValueRef,
    },
    Sub {
        flags: IntArithFlags,
        lhs: ValueRef,
        rhs: ValueRef,
    },
    Mul {
        flags: IntArithFlags,
        lhs: ValueRef,
        rhs: ValueRef,
    },
    UDiv {
        exact: bool,
        lhs: ValueRef,
        rhs: ValueRef,
    },
    SDiv {
        exact: bool,
        lhs: ValueRef,
        rhs: ValueRef,
    },
    URem {
        lhs: ValueRef,
        rhs: ValueRef,
    },
    SRem {
        lhs: ValueRef,
        rhs: ValueRef,
    },

    // --- Bitwise ---
    And {
        lhs: ValueRef,
        rhs: ValueRef,
    },
    Or {
        lhs: ValueRef,
        rhs: ValueRef,
    },
    Xor {
        lhs: ValueRef,
        rhs: ValueRef,
    },
    Shl {
        flags: IntArithFlags,
        lhs: ValueRef,
        rhs: ValueRef,
    },
    LShr {
        exact: bool,
        lhs: ValueRef,
        rhs: ValueRef,
    },
    AShr {
        exact: bool,
        lhs: ValueRef,
        rhs: ValueRef,
    },

    // --- Floating-point arithmetic ---
    FAdd {
        flags: FastMathFlags,
        lhs: ValueRef,
        rhs: ValueRef,
    },
    FSub {
        flags: FastMathFlags,
        lhs: ValueRef,
        rhs: ValueRef,
    },
    FMul {
        flags: FastMathFlags,
        lhs: ValueRef,
        rhs: ValueRef,
    },
    FDiv {
        flags: FastMathFlags,
        lhs: ValueRef,
        rhs: ValueRef,
    },
    FRem {
        flags: FastMathFlags,
        lhs: ValueRef,
        rhs: ValueRef,
    },
    FNeg {
        flags: FastMathFlags,
        operand: ValueRef,
    },

    // --- Comparisons ---
    ICmp {
        pred: IntPredicate,
        lhs: ValueRef,
        rhs: ValueRef,
    },
    FCmp {
        flags: FastMathFlags,
        pred: FloatPredicate,
        lhs: ValueRef,
        rhs: ValueRef,
    },

    // --- Memory ---
    Alloca {
        alloc_ty: TypeId,
        num_elements: Option<ValueRef>,
        align: Option<u32>,
    },
    Load {
        ty: TypeId,
        ptr: ValueRef,
        align: Option<u32>,
        volatile: bool,
    },
    Store {
        val: ValueRef,
        ptr: ValueRef,
        align: Option<u32>,
        volatile: bool,
    },
    GetElementPtr {
        inbounds: bool,
        base_ty: TypeId,
        ptr: ValueRef,
        indices: Vec<ValueRef>,
    },

    // --- Casts ---
    Trunc {
        val: ValueRef,
        to: TypeId,
    },
    ZExt {
        val: ValueRef,
        to: TypeId,
    },
    SExt {
        val: ValueRef,
        to: TypeId,
    },
    FPTrunc {
        val: ValueRef,
        to: TypeId,
    },
    FPExt {
        val: ValueRef,
        to: TypeId,
    },
    FPToUI {
        val: ValueRef,
        to: TypeId,
    },
    FPToSI {
        val: ValueRef,
        to: TypeId,
    },
    UIToFP {
        val: ValueRef,
        to: TypeId,
    },
    SIToFP {
        val: ValueRef,
        to: TypeId,
    },
    PtrToInt {
        val: ValueRef,
        to: TypeId,
    },
    IntToPtr {
        val: ValueRef,
        to: TypeId,
    },
    BitCast {
        val: ValueRef,
        to: TypeId,
    },
    AddrSpaceCast {
        val: ValueRef,
        to: TypeId,
    },

    // --- Misc ---
    Select {
        cond: ValueRef,
        then_val: ValueRef,
        else_val: ValueRef,
    },
    Phi {
        ty: TypeId,
        incoming: Vec<(ValueRef, BlockId)>,
    },
    ExtractValue {
        aggregate: ValueRef,
        indices: Vec<u32>,
    },
    InsertValue {
        aggregate: ValueRef,
        val: ValueRef,
        indices: Vec<u32>,
    },
    ExtractElement {
        vec: ValueRef,
        idx: ValueRef,
    },
    InsertElement {
        vec: ValueRef,
        val: ValueRef,
        idx: ValueRef,
    },
    ShuffleVector {
        v1: ValueRef,
        v2: ValueRef,
        mask: Vec<i32>,
    },

    // --- Call ---
    Call {
        tail: TailCallKind,
        callee_ty: TypeId,
        callee: ValueRef,
        args: Vec<ValueRef>,
    },

    // --- Terminators ---
    Ret {
        val: Option<ValueRef>,
    },
    Br {
        dest: BlockId,
    },
    CondBr {
        cond: ValueRef,
        then_dest: BlockId,
        else_dest: BlockId,
    },
    Switch {
        val: ValueRef,
        default: BlockId,
        cases: Vec<(ValueRef, BlockId)>,
    },
    Unreachable,
}

impl InstrKind {
    pub fn is_terminator(&self) -> bool {
        matches!(
            self,
            InstrKind::Ret { .. }
                | InstrKind::Br { .. }
                | InstrKind::CondBr { .. }
                | InstrKind::Switch { .. }
                | InstrKind::Unreachable
        )
    }

    /// Return the opcode name for printing.
    pub fn opcode(&self) -> &'static str {
        match self {
            InstrKind::Add { .. } => "add",
            InstrKind::Sub { .. } => "sub",
            InstrKind::Mul { .. } => "mul",
            InstrKind::UDiv { .. } => "udiv",
            InstrKind::SDiv { .. } => "sdiv",
            InstrKind::URem { .. } => "urem",
            InstrKind::SRem { .. } => "srem",
            InstrKind::And { .. } => "and",
            InstrKind::Or { .. } => "or",
            InstrKind::Xor { .. } => "xor",
            InstrKind::Shl { .. } => "shl",
            InstrKind::LShr { .. } => "lshr",
            InstrKind::AShr { .. } => "ashr",
            InstrKind::FAdd { .. } => "fadd",
            InstrKind::FSub { .. } => "fsub",
            InstrKind::FMul { .. } => "fmul",
            InstrKind::FDiv { .. } => "fdiv",
            InstrKind::FRem { .. } => "frem",
            InstrKind::FNeg { .. } => "fneg",
            InstrKind::ICmp { .. } => "icmp",
            InstrKind::FCmp { .. } => "fcmp",
            InstrKind::Alloca { .. } => "alloca",
            InstrKind::Load { .. } => "load",
            InstrKind::Store { .. } => "store",
            InstrKind::GetElementPtr { .. } => "getelementptr",
            InstrKind::Trunc { .. } => "trunc",
            InstrKind::ZExt { .. } => "zext",
            InstrKind::SExt { .. } => "sext",
            InstrKind::FPTrunc { .. } => "fptrunc",
            InstrKind::FPExt { .. } => "fpext",
            InstrKind::FPToUI { .. } => "fptoui",
            InstrKind::FPToSI { .. } => "fptosi",
            InstrKind::UIToFP { .. } => "uitofp",
            InstrKind::SIToFP { .. } => "sitofp",
            InstrKind::PtrToInt { .. } => "ptrtoint",
            InstrKind::IntToPtr { .. } => "inttoptr",
            InstrKind::BitCast { .. } => "bitcast",
            InstrKind::AddrSpaceCast { .. } => "addrspacecast",
            InstrKind::Select { .. } => "select",
            InstrKind::Phi { .. } => "phi",
            InstrKind::ExtractValue { .. } => "extractvalue",
            InstrKind::InsertValue { .. } => "insertvalue",
            InstrKind::ExtractElement { .. } => "extractelement",
            InstrKind::InsertElement { .. } => "insertelement",
            InstrKind::ShuffleVector { .. } => "shufflevector",
            InstrKind::Call { .. } => "call",
            InstrKind::Ret { .. } => "ret",
            InstrKind::Br { .. } => "br",
            InstrKind::CondBr { .. } => "br",
            InstrKind::Switch { .. } => "switch",
            InstrKind::Unreachable => "unreachable",
        }
    }

    /// Collect all `ValueRef` operands (not including block successors).
    pub fn operands(&self) -> Vec<ValueRef> {
        match self {
            InstrKind::Add { lhs, rhs, .. }
            | InstrKind::Sub { lhs, rhs, .. }
            | InstrKind::Mul { lhs, rhs, .. }
            | InstrKind::UDiv { lhs, rhs, .. }
            | InstrKind::SDiv { lhs, rhs, .. }
            | InstrKind::URem { lhs, rhs }
            | InstrKind::SRem { lhs, rhs }
            | InstrKind::And { lhs, rhs }
            | InstrKind::Or { lhs, rhs }
            | InstrKind::Xor { lhs, rhs }
            | InstrKind::Shl { lhs, rhs, .. }
            | InstrKind::LShr { lhs, rhs, .. }
            | InstrKind::AShr { lhs, rhs, .. }
            | InstrKind::FAdd { lhs, rhs, .. }
            | InstrKind::FSub { lhs, rhs, .. }
            | InstrKind::FMul { lhs, rhs, .. }
            | InstrKind::FDiv { lhs, rhs, .. }
            | InstrKind::FRem { lhs, rhs, .. }
            | InstrKind::ICmp { lhs, rhs, .. }
            | InstrKind::FCmp { lhs, rhs, .. } => vec![*lhs, *rhs],

            InstrKind::FNeg { operand, .. } => vec![*operand],

            InstrKind::Alloca { num_elements, .. } => num_elements.iter().copied().collect(),
            InstrKind::Load { ptr, .. } => vec![*ptr],
            InstrKind::Store { val, ptr, .. } => vec![*val, *ptr],
            InstrKind::GetElementPtr { ptr, indices, .. } => {
                let mut v = vec![*ptr];
                v.extend_from_slice(indices);
                v
            }

            InstrKind::Trunc { val, .. }
            | InstrKind::ZExt { val, .. }
            | InstrKind::SExt { val, .. }
            | InstrKind::FPTrunc { val, .. }
            | InstrKind::FPExt { val, .. }
            | InstrKind::FPToUI { val, .. }
            | InstrKind::FPToSI { val, .. }
            | InstrKind::UIToFP { val, .. }
            | InstrKind::SIToFP { val, .. }
            | InstrKind::PtrToInt { val, .. }
            | InstrKind::IntToPtr { val, .. }
            | InstrKind::BitCast { val, .. }
            | InstrKind::AddrSpaceCast { val, .. } => vec![*val],

            InstrKind::Select {
                cond,
                then_val,
                else_val,
            } => {
                vec![*cond, *then_val, *else_val]
            }
            InstrKind::Phi { incoming, .. } => incoming.iter().map(|(v, _)| *v).collect(),
            InstrKind::ExtractValue { aggregate, .. } => vec![*aggregate],
            InstrKind::InsertValue { aggregate, val, .. } => vec![*aggregate, *val],
            InstrKind::ExtractElement { vec, idx } => vec![*vec, *idx],
            InstrKind::InsertElement { vec, val, idx } => vec![*vec, *val, *idx],
            InstrKind::ShuffleVector { v1, v2, .. } => vec![*v1, *v2],
            InstrKind::Call { callee, args, .. } => {
                let mut v = vec![*callee];
                v.extend_from_slice(args);
                v
            }
            InstrKind::Ret { val } => val.iter().copied().collect(),
            InstrKind::Br { .. } | InstrKind::Unreachable => vec![],
            InstrKind::CondBr { cond, .. } => vec![*cond],
            InstrKind::Switch { val, cases, .. } => {
                let mut v = vec![*val];
                for (case_val, _) in cases {
                    v.push(*case_val);
                }
                v
            }
        }
    }

    /// Return successor block ids (for terminators).
    ///
    /// This match is intentionally exhaustive (no wildcard) so that adding a
    /// new `InstrKind` variant without updating `successors()` is a
    /// **compile error**, preventing silent CFG omissions.
    pub fn successors(&self) -> Vec<BlockId> {
        match self {
            InstrKind::Br { dest } => vec![*dest],
            InstrKind::CondBr {
                then_dest,
                else_dest,
                ..
            } => {
                vec![*then_dest, *else_dest]
            }
            InstrKind::Switch { default, cases, .. } => {
                let mut v = vec![*default];
                for (_, bb) in cases {
                    v.push(*bb);
                }
                v
            }
            // Non-terminators and exit terminators (Ret, Unreachable) have no
            // successors.  Listed explicitly so the compiler enforces that every
            // future variant is consciously placed in one arm or the other.
            InstrKind::Ret { .. }
            | InstrKind::Unreachable
            | InstrKind::Add { .. }
            | InstrKind::Sub { .. }
            | InstrKind::Mul { .. }
            | InstrKind::UDiv { .. }
            | InstrKind::SDiv { .. }
            | InstrKind::URem { .. }
            | InstrKind::SRem { .. }
            | InstrKind::And { .. }
            | InstrKind::Or { .. }
            | InstrKind::Xor { .. }
            | InstrKind::Shl { .. }
            | InstrKind::LShr { .. }
            | InstrKind::AShr { .. }
            | InstrKind::FAdd { .. }
            | InstrKind::FSub { .. }
            | InstrKind::FMul { .. }
            | InstrKind::FDiv { .. }
            | InstrKind::FRem { .. }
            | InstrKind::FNeg { .. }
            | InstrKind::ICmp { .. }
            | InstrKind::FCmp { .. }
            | InstrKind::Alloca { .. }
            | InstrKind::Load { .. }
            | InstrKind::Store { .. }
            | InstrKind::GetElementPtr { .. }
            | InstrKind::Trunc { .. }
            | InstrKind::ZExt { .. }
            | InstrKind::SExt { .. }
            | InstrKind::FPTrunc { .. }
            | InstrKind::FPExt { .. }
            | InstrKind::FPToUI { .. }
            | InstrKind::FPToSI { .. }
            | InstrKind::UIToFP { .. }
            | InstrKind::SIToFP { .. }
            | InstrKind::PtrToInt { .. }
            | InstrKind::IntToPtr { .. }
            | InstrKind::BitCast { .. }
            | InstrKind::AddrSpaceCast { .. }
            | InstrKind::Select { .. }
            | InstrKind::Phi { .. }
            | InstrKind::ExtractValue { .. }
            | InstrKind::InsertValue { .. }
            | InstrKind::ExtractElement { .. }
            | InstrKind::InsertElement { .. }
            | InstrKind::ShuffleVector { .. }
            | InstrKind::Call { .. } => vec![],
        }
    }
}

/// A single SSA instruction.
#[derive(Clone, Debug)]
pub struct Instruction {
    /// Optional result name (None for void or unnamed).
    pub name: Option<String>,
    /// Type of the result (void_ty for void instructions and terminators).
    pub ty: TypeId,
    pub kind: InstrKind,
}

impl Instruction {
    pub fn new(name: Option<String>, ty: TypeId, kind: InstrKind) -> Self {
        Instruction { name, ty, kind }
    }

    pub fn is_terminator(&self) -> bool {
        self.kind.is_terminator()
    }

    pub fn operands(&self) -> Vec<ValueRef> {
        self.kind.operands()
    }

    pub fn successors(&self) -> Vec<BlockId> {
        self.kind.successors()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{BlockId, ConstId, Context, InstrId};

    // ── tiny helpers ────────────────────────────────────────────────────────
    fn c0() -> ValueRef { ValueRef::Constant(ConstId(0)) }
    fn c1() -> ValueRef { ValueRef::Constant(ConstId(1)) }
    fn c2() -> ValueRef { ValueRef::Constant(ConstId(2)) }
    fn v0() -> ValueRef { ValueRef::Instruction(InstrId(0)) }
    fn b0() -> BlockId  { BlockId(0) }
    fn b1() -> BlockId  { BlockId(1) }

    // ── existing tests ───────────────────────────────────────────────────────

    #[test]
    fn terminator_check() {
        let _ctx = Context::new();
        let ret = InstrKind::Ret { val: None };
        assert!(ret.is_terminator());
        let add = InstrKind::Add {
            flags: IntArithFlags::default(),
            lhs: c0(),
            rhs: c0(),
        };
        assert!(!add.is_terminator());
    }

    #[test]
    fn opcode_names() {
        assert_eq!(InstrKind::Unreachable.opcode(), "unreachable");
        let br = InstrKind::Br { dest: b0() };
        assert_eq!(br.opcode(), "br");
    }

    // ── operands() — integer arithmetic (7 variants) ─────────────────────────

    #[test]
    fn operands_add() {
        let k = InstrKind::Add { flags: IntArithFlags::default(), lhs: c0(), rhs: c1() };
        assert_eq!(k.operands(), vec![c0(), c1()]);
    }

    #[test]
    fn operands_sub() {
        let k = InstrKind::Sub { flags: IntArithFlags::default(), lhs: c0(), rhs: c1() };
        assert_eq!(k.operands(), vec![c0(), c1()]);
    }

    #[test]
    fn operands_mul() {
        let k = InstrKind::Mul { flags: IntArithFlags::default(), lhs: c0(), rhs: c1() };
        assert_eq!(k.operands(), vec![c0(), c1()]);
    }

    #[test]
    fn operands_udiv() {
        let k = InstrKind::UDiv { exact: false, lhs: c0(), rhs: c1() };
        assert_eq!(k.operands(), vec![c0(), c1()]);
    }

    #[test]
    fn operands_sdiv() {
        let k = InstrKind::SDiv { exact: true, lhs: c0(), rhs: c1() };
        assert_eq!(k.operands(), vec![c0(), c1()]);
    }

    #[test]
    fn operands_urem() {
        let k = InstrKind::URem { lhs: c0(), rhs: c1() };
        assert_eq!(k.operands(), vec![c0(), c1()]);
    }

    #[test]
    fn operands_srem() {
        let k = InstrKind::SRem { lhs: c0(), rhs: c1() };
        assert_eq!(k.operands(), vec![c0(), c1()]);
    }

    // ── operands() — bitwise (6 variants) ─────────────────────────────────────

    #[test]
    fn operands_and() {
        assert_eq!(InstrKind::And { lhs: c0(), rhs: c1() }.operands(), vec![c0(), c1()]);
    }

    #[test]
    fn operands_or() {
        assert_eq!(InstrKind::Or { lhs: c0(), rhs: c1() }.operands(), vec![c0(), c1()]);
    }

    #[test]
    fn operands_xor() {
        assert_eq!(InstrKind::Xor { lhs: c0(), rhs: c1() }.operands(), vec![c0(), c1()]);
    }

    #[test]
    fn operands_shl() {
        let k = InstrKind::Shl { flags: IntArithFlags::default(), lhs: c0(), rhs: c1() };
        assert_eq!(k.operands(), vec![c0(), c1()]);
    }

    #[test]
    fn operands_lshr() {
        assert_eq!(InstrKind::LShr { exact: false, lhs: c0(), rhs: c1() }.operands(), vec![c0(), c1()]);
    }

    #[test]
    fn operands_ashr() {
        assert_eq!(InstrKind::AShr { exact: false, lhs: c0(), rhs: c1() }.operands(), vec![c0(), c1()]);
    }

    // ── operands() — FP arithmetic (6 variants) ───────────────────────────────

    #[test]
    fn operands_fadd() {
        let k = InstrKind::FAdd { flags: FastMathFlags::default(), lhs: c0(), rhs: c1() };
        assert_eq!(k.operands(), vec![c0(), c1()]);
    }

    #[test]
    fn operands_fsub() {
        let k = InstrKind::FSub { flags: FastMathFlags::default(), lhs: c0(), rhs: c1() };
        assert_eq!(k.operands(), vec![c0(), c1()]);
    }

    #[test]
    fn operands_fmul() {
        let k = InstrKind::FMul { flags: FastMathFlags::default(), lhs: c0(), rhs: c1() };
        assert_eq!(k.operands(), vec![c0(), c1()]);
    }

    #[test]
    fn operands_fdiv() {
        let k = InstrKind::FDiv { flags: FastMathFlags::default(), lhs: c0(), rhs: c1() };
        assert_eq!(k.operands(), vec![c0(), c1()]);
    }

    #[test]
    fn operands_frem() {
        let k = InstrKind::FRem { flags: FastMathFlags::default(), lhs: c0(), rhs: c1() };
        assert_eq!(k.operands(), vec![c0(), c1()]);
    }

    #[test]
    fn operands_fneg() {
        let k = InstrKind::FNeg { flags: FastMathFlags::default(), operand: c0() };
        assert_eq!(k.operands(), vec![c0()]);
    }

    // ── operands() — comparisons (2 variants) ────────────────────────────────

    #[test]
    fn operands_icmp() {
        let k = InstrKind::ICmp { pred: IntPredicate::Eq, lhs: c0(), rhs: c1() };
        assert_eq!(k.operands(), vec![c0(), c1()]);
    }

    #[test]
    fn operands_fcmp() {
        let k = InstrKind::FCmp {
            flags: FastMathFlags::default(),
            pred: FloatPredicate::Oeq,
            lhs: c0(),
            rhs: c1(),
        };
        assert_eq!(k.operands(), vec![c0(), c1()]);
    }

    // ── operands() — memory (4 variants) ─────────────────────────────────────

    #[test]
    fn operands_alloca_no_count() {
        let ctx = Context::new();
        let k = InstrKind::Alloca { alloc_ty: ctx.i32_ty, num_elements: None, align: None };
        assert_eq!(k.operands(), vec![]);
    }

    #[test]
    fn operands_alloca_with_count() {
        let ctx = Context::new();
        let k = InstrKind::Alloca { alloc_ty: ctx.i32_ty, num_elements: Some(c0()), align: None };
        assert_eq!(k.operands(), vec![c0()]);
    }

    #[test]
    fn operands_load() {
        let ctx = Context::new();
        let k = InstrKind::Load { ty: ctx.i32_ty, ptr: c0(), align: None, volatile: false };
        assert_eq!(k.operands(), vec![c0()]);
    }

    #[test]
    fn operands_store() {
        let k = InstrKind::Store { val: c0(), ptr: c1(), align: None, volatile: false };
        assert_eq!(k.operands(), vec![c0(), c1()]);
    }

    #[test]
    fn operands_gep_no_indices() {
        let ctx = Context::new();
        let k = InstrKind::GetElementPtr {
            inbounds: false,
            base_ty: ctx.i32_ty,
            ptr: c0(),
            indices: vec![],
        };
        assert_eq!(k.operands(), vec![c0()]);
    }

    #[test]
    fn operands_gep_two_indices() {
        let ctx = Context::new();
        let k = InstrKind::GetElementPtr {
            inbounds: true,
            base_ty: ctx.i32_ty,
            ptr: c0(),
            indices: vec![c1(), v0()],
        };
        assert_eq!(k.operands(), vec![c0(), c1(), v0()]);
    }

    // ── operands() — casts (13 variants) ─────────────────────────────────────

    #[test]
    fn operands_trunc() {
        let ctx = Context::new();
        assert_eq!(InstrKind::Trunc { val: c0(), to: ctx.i8_ty }.operands(), vec![c0()]);
    }

    #[test]
    fn operands_zext() {
        let ctx = Context::new();
        assert_eq!(InstrKind::ZExt { val: c0(), to: ctx.i64_ty }.operands(), vec![c0()]);
    }

    #[test]
    fn operands_sext() {
        let ctx = Context::new();
        assert_eq!(InstrKind::SExt { val: c0(), to: ctx.i64_ty }.operands(), vec![c0()]);
    }

    #[test]
    fn operands_fptrunc() {
        let ctx = Context::new();
        let f32_ty = ctx.f32_ty;
        assert_eq!(InstrKind::FPTrunc { val: c0(), to: f32_ty }.operands(), vec![c0()]);
    }

    #[test]
    fn operands_fpext() {
        let ctx = Context::new();
        let f64_ty = ctx.f64_ty;
        assert_eq!(InstrKind::FPExt { val: c0(), to: f64_ty }.operands(), vec![c0()]);
    }

    #[test]
    fn operands_fptoui() {
        let ctx = Context::new();
        assert_eq!(InstrKind::FPToUI { val: c0(), to: ctx.i64_ty }.operands(), vec![c0()]);
    }

    #[test]
    fn operands_fptosi() {
        let ctx = Context::new();
        assert_eq!(InstrKind::FPToSI { val: c0(), to: ctx.i64_ty }.operands(), vec![c0()]);
    }

    #[test]
    fn operands_uitofp() {
        let ctx = Context::new();
        let f64_ty = ctx.f64_ty;
        assert_eq!(InstrKind::UIToFP { val: c0(), to: f64_ty }.operands(), vec![c0()]);
    }

    #[test]
    fn operands_sitofp() {
        let ctx = Context::new();
        let f64_ty = ctx.f64_ty;
        assert_eq!(InstrKind::SIToFP { val: c0(), to: f64_ty }.operands(), vec![c0()]);
    }

    #[test]
    fn operands_ptrtoint() {
        let ctx = Context::new();
        assert_eq!(InstrKind::PtrToInt { val: c0(), to: ctx.i64_ty }.operands(), vec![c0()]);
    }

    #[test]
    fn operands_inttoptr() {
        let ctx = Context::new();
        let ptr_ty = ctx.ptr_ty;
        assert_eq!(InstrKind::IntToPtr { val: c0(), to: ptr_ty }.operands(), vec![c0()]);
    }

    #[test]
    fn operands_bitcast() {
        let ctx = Context::new();
        let ptr_ty = ctx.ptr_ty;
        assert_eq!(InstrKind::BitCast { val: c0(), to: ptr_ty }.operands(), vec![c0()]);
    }

    #[test]
    fn operands_addrspacecast() {
        let ctx = Context::new();
        let ptr_ty = ctx.ptr_ty;
        assert_eq!(InstrKind::AddrSpaceCast { val: c0(), to: ptr_ty }.operands(), vec![c0()]);
    }

    // ── operands() — misc (7 variants) ───────────────────────────────────────

    #[test]
    fn operands_select() {
        let k = InstrKind::Select { cond: c0(), then_val: c1(), else_val: v0() };
        assert_eq!(k.operands(), vec![c0(), c1(), v0()]);
    }

    #[test]
    fn operands_phi_empty() {
        let ctx = Context::new();
        let k = InstrKind::Phi { ty: ctx.i32_ty, incoming: vec![] };
        assert_eq!(k.operands(), vec![]);
    }

    #[test]
    fn operands_phi_two_incoming() {
        let ctx = Context::new();
        // Only value refs, not block ids, must appear in operands().
        let k = InstrKind::Phi {
            ty: ctx.i32_ty,
            incoming: vec![(c0(), b0()), (c1(), b1())],
        };
        assert_eq!(k.operands(), vec![c0(), c1()]);
    }

    #[test]
    fn operands_extractvalue() {
        let k = InstrKind::ExtractValue { aggregate: c0(), indices: vec![0, 1] };
        assert_eq!(k.operands(), vec![c0()]);
    }

    #[test]
    fn operands_insertvalue() {
        let k = InstrKind::InsertValue { aggregate: c0(), val: c1(), indices: vec![0] };
        assert_eq!(k.operands(), vec![c0(), c1()]);
    }

    #[test]
    fn operands_extractelement() {
        let k = InstrKind::ExtractElement { vec: c0(), idx: c1() };
        assert_eq!(k.operands(), vec![c0(), c1()]);
    }

    #[test]
    fn operands_insertelement() {
        let k = InstrKind::InsertElement { vec: c0(), val: c1(), idx: v0() };
        assert_eq!(k.operands(), vec![c0(), c1(), v0()]);
    }

    #[test]
    fn operands_shufflevector() {
        // mask is Vec<i32> (integer literals, not ValueRefs) — v1 and v2 only.
        let k = InstrKind::ShuffleVector { v1: c0(), v2: c1(), mask: vec![0, 1, 0, 1] };
        assert_eq!(k.operands(), vec![c0(), c1()]);
    }

    // ── operands() — call (1 variant, 2 cases) ────────────────────────────────

    #[test]
    fn operands_call_no_args() {
        let mut ctx = Context::new();
        let callee_ty = ctx.mk_fn_type(ctx.void_ty, vec![], false);
        let k = InstrKind::Call {
            tail: TailCallKind::None,
            callee_ty,
            callee: c0(),
            args: vec![],
        };
        // callee is always first, then args.
        assert_eq!(k.operands(), vec![c0()]);
    }

    #[test]
    fn operands_call_with_args() {
        let mut ctx = Context::new();
        let callee_ty = ctx.mk_fn_type(ctx.void_ty, vec![ctx.i32_ty, ctx.i32_ty], false);
        let k = InstrKind::Call {
            tail: TailCallKind::None,
            callee_ty,
            callee: c0(),
            args: vec![c1(), c2()],
        };
        assert_eq!(k.operands(), vec![c0(), c1(), c2()]);
    }

    // ── operands() — terminators (5 variants) ────────────────────────────────

    #[test]
    fn operands_ret_void() {
        assert_eq!(InstrKind::Ret { val: None }.operands(), vec![]);
    }

    #[test]
    fn operands_ret_value() {
        assert_eq!(InstrKind::Ret { val: Some(c0()) }.operands(), vec![c0()]);
    }

    #[test]
    fn operands_br() {
        // Unconditional branch has no value operands.
        assert_eq!(InstrKind::Br { dest: b0() }.operands(), vec![]);
    }

    #[test]
    fn operands_condbr() {
        let k = InstrKind::CondBr { cond: c0(), then_dest: b0(), else_dest: b1() };
        // Only the condition is a value operand; block targets are not.
        assert_eq!(k.operands(), vec![c0()]);
    }

    #[test]
    fn operands_switch_two_cases() {
        // val + all case values; block targets are not operands.
        let k = InstrKind::Switch {
            val: c0(),
            default: b0(),
            cases: vec![(c1(), b1()), (c2(), b0())],
        };
        assert_eq!(k.operands(), vec![c0(), c1(), c2()]);
    }

    #[test]
    fn operands_unreachable() {
        assert_eq!(InstrKind::Unreachable.operands(), vec![]);
    }

    // ── successors() — terminators ────────────────────────────────────────────

    #[test]
    fn successors_br() {
        assert_eq!(InstrKind::Br { dest: b0() }.successors(), vec![b0()]);
    }

    #[test]
    fn successors_condbr() {
        let k = InstrKind::CondBr { cond: c0(), then_dest: b0(), else_dest: b1() };
        assert_eq!(k.successors(), vec![b0(), b1()]);
    }

    #[test]
    fn successors_switch_default_plus_cases() {
        let k = InstrKind::Switch {
            val: c0(),
            default: b0(),
            cases: vec![(c1(), b1()), (c2(), b0())],
        };
        assert_eq!(k.successors(), vec![b0(), b1(), b0()]);
    }

    #[test]
    fn successors_switch_no_cases() {
        let k = InstrKind::Switch { val: c0(), default: b1(), cases: vec![] };
        assert_eq!(k.successors(), vec![b1()]);
    }

    #[test]
    fn successors_ret_void() {
        assert_eq!(InstrKind::Ret { val: None }.successors(), vec![]);
    }

    #[test]
    fn successors_ret_value() {
        assert_eq!(InstrKind::Ret { val: Some(c0()) }.successors(), vec![]);
    }

    #[test]
    fn successors_unreachable() {
        assert_eq!(InstrKind::Unreachable.successors(), vec![]);
    }

    // ── successors() — non-terminators all return empty ───────────────────────

    #[test]
    fn successors_non_terminators_are_empty() {
        let mut ctx = Context::new();
        let callee_ty = ctx.mk_fn_type(ctx.void_ty, vec![], false);
        // One representative from each group of non-terminators.
        let cases: &[InstrKind] = &[
            InstrKind::Add { flags: IntArithFlags::default(), lhs: c0(), rhs: c1() },
            InstrKind::Sub { flags: IntArithFlags::default(), lhs: c0(), rhs: c1() },
            InstrKind::Mul { flags: IntArithFlags::default(), lhs: c0(), rhs: c1() },
            InstrKind::UDiv { exact: false, lhs: c0(), rhs: c1() },
            InstrKind::SDiv { exact: false, lhs: c0(), rhs: c1() },
            InstrKind::URem { lhs: c0(), rhs: c1() },
            InstrKind::SRem { lhs: c0(), rhs: c1() },
            InstrKind::And { lhs: c0(), rhs: c1() },
            InstrKind::Or  { lhs: c0(), rhs: c1() },
            InstrKind::Xor { lhs: c0(), rhs: c1() },
            InstrKind::Shl { flags: IntArithFlags::default(), lhs: c0(), rhs: c1() },
            InstrKind::LShr { exact: false, lhs: c0(), rhs: c1() },
            InstrKind::AShr { exact: false, lhs: c0(), rhs: c1() },
            InstrKind::FAdd { flags: FastMathFlags::default(), lhs: c0(), rhs: c1() },
            InstrKind::FSub { flags: FastMathFlags::default(), lhs: c0(), rhs: c1() },
            InstrKind::FMul { flags: FastMathFlags::default(), lhs: c0(), rhs: c1() },
            InstrKind::FDiv { flags: FastMathFlags::default(), lhs: c0(), rhs: c1() },
            InstrKind::FRem { flags: FastMathFlags::default(), lhs: c0(), rhs: c1() },
            InstrKind::FNeg { flags: FastMathFlags::default(), operand: c0() },
            InstrKind::ICmp { pred: IntPredicate::Eq, lhs: c0(), rhs: c1() },
            InstrKind::FCmp { flags: FastMathFlags::default(), pred: FloatPredicate::Oeq, lhs: c0(), rhs: c1() },
            InstrKind::Alloca { alloc_ty: ctx.i32_ty, num_elements: None, align: None },
            InstrKind::Load  { ty: ctx.i32_ty, ptr: c0(), align: None, volatile: false },
            InstrKind::Store { val: c0(), ptr: c1(), align: None, volatile: false },
            InstrKind::GetElementPtr { inbounds: false, base_ty: ctx.i32_ty, ptr: c0(), indices: vec![] },
            InstrKind::Trunc { val: c0(), to: ctx.i8_ty },
            InstrKind::ZExt  { val: c0(), to: ctx.i64_ty },
            InstrKind::SExt  { val: c0(), to: ctx.i64_ty },
            InstrKind::FPTrunc { val: c0(), to: ctx.f32_ty },
            InstrKind::FPExt   { val: c0(), to: ctx.f64_ty },
            InstrKind::FPToUI  { val: c0(), to: ctx.i64_ty },
            InstrKind::FPToSI  { val: c0(), to: ctx.i64_ty },
            InstrKind::UIToFP  { val: c0(), to: ctx.f64_ty },
            InstrKind::SIToFP  { val: c0(), to: ctx.f64_ty },
            InstrKind::PtrToInt { val: c0(), to: ctx.i64_ty },
            InstrKind::IntToPtr { val: c0(), to: ctx.ptr_ty },
            InstrKind::BitCast  { val: c0(), to: ctx.ptr_ty },
            InstrKind::AddrSpaceCast { val: c0(), to: ctx.ptr_ty },
            InstrKind::Select { cond: c0(), then_val: c1(), else_val: v0() },
            InstrKind::Phi { ty: ctx.i32_ty, incoming: vec![] },
            InstrKind::ExtractValue { aggregate: c0(), indices: vec![0] },
            InstrKind::InsertValue  { aggregate: c0(), val: c1(), indices: vec![0] },
            InstrKind::ExtractElement { vec: c0(), idx: c1() },
            InstrKind::InsertElement  { vec: c0(), val: c1(), idx: v0() },
            InstrKind::ShuffleVector  { v1: c0(), v2: c1(), mask: vec![0, 1] },
            InstrKind::Call { tail: TailCallKind::None, callee_ty, callee: c0(), args: vec![] },
        ];
        for k in cases {
            assert_eq!(k.successors(), vec![], "{:?} should have no successors", k);
        }
    }
}
