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
            _ => vec![],
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
    use crate::context::Context;

    #[test]
    fn terminator_check() {
        let _ctx = Context::new();
        let ret = InstrKind::Ret { val: None };
        assert!(ret.is_terminator());
        let add = InstrKind::Add {
            flags: IntArithFlags::default(),
            lhs: ValueRef::Constant(crate::context::ConstId(0)),
            rhs: ValueRef::Constant(crate::context::ConstId(0)),
        };
        assert!(!add.is_terminator());
    }

    #[test]
    fn opcode_names() {
        assert_eq!(InstrKind::Unreachable.opcode(), "unreachable");
        let br = InstrKind::Br { dest: BlockId(0) };
        assert_eq!(br.opcode(), "br");
    }
}
