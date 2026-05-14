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
    /// Public API for `exact`.
    pub exact: bool,
}

/// Fast-math flags for floating-point instructions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FastMathFlags {
    /// Public API for `nnan`.
    pub nnan: bool,
    /// Public API for `ninf`.
    pub ninf: bool,
    /// Public API for `nsz`.
    pub nsz: bool,
    /// Public API for `arcp`.
    pub arcp: bool,
    /// Public API for `contract`.
    pub contract: bool,
    /// Public API for `afn`.
    pub afn: bool,
    /// Public API for `reassoc`.
    pub reassoc: bool,
    /// Shorthand for all flags set.
    pub fast: bool,
}

// ---------------------------------------------------------------------------
// Predicates
// ---------------------------------------------------------------------------

/// Public API for `IntPredicate`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IntPredicate {
    /// `Eq` variant.
    Eq,
    /// `Ne` variant.
    Ne,
    /// `Ugt` variant.
    Ugt,
    /// `Uge` variant.
    Uge,
    /// `Ult` variant.
    Ult,
    /// `Ule` variant.
    Ule,
    /// `Sgt` variant.
    Sgt,
    /// `Sge` variant.
    Sge,
    /// `Slt` variant.
    Slt,
    /// `Sle` variant.
    Sle,
}

impl IntPredicate {
    /// Public API for `as_str`.
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

/// Public API for `FloatPredicate`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FloatPredicate {
    /// `False` variant.
    False,
    /// `Oeq` variant.
    Oeq,
    /// `Ogt` variant.
    Ogt,
    /// `Oge` variant.
    Oge,
    /// `Olt` variant.
    Olt,
    /// `Ole` variant.
    Ole,
    /// `One` variant.
    One,
    /// `Ord` variant.
    Ord,
    /// `Uno` variant.
    Uno,
    /// `Ueq` variant.
    Ueq,
    /// `Ugt` variant.
    Ugt,
    /// `Uge` variant.
    Uge,
    /// `Ult` variant.
    Ult,
    /// `Ule` variant.
    Ule,
    /// `Une` variant.
    Une,
    /// `True` variant.
    True,
}

impl FloatPredicate {
    /// Public API for `as_str`.
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
    /// `None` variant.
    None,
    /// `Tail` variant.
    Tail,
    /// `MustTail` variant.
    MustTail,
    /// `NoTail` variant.
    NoTail,
}

/// Memory ordering for atomic instructions (fence / cmpxchg / atomicrmw).
///
/// Variants follow the LLVM IR spelling.  Stronger orderings are towards the
/// bottom of the list, matching LLVM's documented `AtomicOrdering` enum.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemOrdering {
    /// `unordered` — no atomicity, racing reads/writes are UB at the language level.
    Unordered,
    /// `monotonic` — atomic load/store with no ordering wrt other locations.
    Monotonic,
    /// `acquire` — load-acquire.
    Acquire,
    /// `release` — store-release.
    Release,
    /// `acq_rel` — acquire on the load half, release on the store half.
    AcqRel,
    /// `seq_cst` — sequentially consistent.
    SeqCst,
}

impl MemOrdering {
    /// Textual IR spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            MemOrdering::Unordered => "unordered",
            MemOrdering::Monotonic => "monotonic",
            MemOrdering::Acquire => "acquire",
            MemOrdering::Release => "release",
            MemOrdering::AcqRel => "acq_rel",
            MemOrdering::SeqCst => "seq_cst",
        }
    }
}

/// Operation kind for `atomicrmw`.
///
/// Mirrors LLVM's `AtomicRMWInst::BinOp`.  Each variant pairs the operation
/// with the existing value at the target pointer and writes the result back
/// atomically; the instruction's result is the *old* value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RmwOp {
    /// Replace the value (swap).
    Xchg,
    /// Integer add.
    Add,
    /// Integer subtract.
    Sub,
    /// Bitwise and.
    And,
    /// Bitwise nand (`!(old & val)`).
    Nand,
    /// Bitwise or.
    Or,
    /// Bitwise xor.
    Xor,
    /// Signed max.
    Max,
    /// Signed min.
    Min,
    /// Unsigned max.
    UMax,
    /// Unsigned min.
    UMin,
    /// Floating-point add.
    FAdd,
    /// Floating-point subtract.
    FSub,
}

/// Recognized LLVM vector-predication intrinsic families.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum VpIntrinsic {
    /// `llvm.vp.add.*`.
    Add,
    /// `llvm.vp.sub.*`.
    Sub,
    /// `llvm.vp.mul.*`.
    Mul,
    /// `llvm.vp.sdiv.*`.
    SDiv,
    /// `llvm.vp.udiv.*`.
    UDiv,
    /// `llvm.vp.and.*`.
    And,
    /// `llvm.vp.or.*`.
    Or,
    /// `llvm.vp.xor.*`.
    Xor,
    /// `llvm.vp.shl.*`.
    Shl,
    /// `llvm.vp.lshr.*`.
    LShr,
    /// `llvm.vp.ashr.*`.
    AShr,
    /// `llvm.vp.load.*`.
    Load,
    /// `llvm.vp.store.*`.
    Store,
    /// `llvm.vp.reduce.add.*`.
    ReduceAdd,
}

impl VpIntrinsic {
    /// Recognize an LLVM `vp.*` intrinsic symbol name.
    pub fn from_name(name: &str) -> Option<Self> {
        let rest = name.strip_prefix("llvm.vp.")?;
        let op = rest.split('.').next()?;
        Some(match op {
            "add" => Self::Add,
            "sub" => Self::Sub,
            "mul" => Self::Mul,
            "sdiv" => Self::SDiv,
            "udiv" => Self::UDiv,
            "and" => Self::And,
            "or" => Self::Or,
            "xor" => Self::Xor,
            "shl" => Self::Shl,
            "lshr" => Self::LShr,
            "ashr" => Self::AShr,
            "load" => Self::Load,
            "store" => Self::Store,
            "reduce" if rest.starts_with("reduce.add.") => Self::ReduceAdd,
            _ => return None,
        })
    }
}

impl RmwOp {
    /// Textual IR spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            RmwOp::Xchg => "xchg",
            RmwOp::Add => "add",
            RmwOp::Sub => "sub",
            RmwOp::And => "and",
            RmwOp::Nand => "nand",
            RmwOp::Or => "or",
            RmwOp::Xor => "xor",
            RmwOp::Max => "max",
            RmwOp::Min => "min",
            RmwOp::UMax => "umax",
            RmwOp::UMin => "umin",
            RmwOp::FAdd => "fadd",
            RmwOp::FSub => "fsub",
        }
    }
}

// ---------------------------------------------------------------------------
// Instruction kind
// ---------------------------------------------------------------------------

/// Public API for `InstrKind`.
#[derive(Clone, Debug, PartialEq)]
pub enum InstrKind {
    // --- Integer arithmetic ---
    /// `Add` variant.
    Add {
        flags: IntArithFlags,
        lhs: ValueRef,
        rhs: ValueRef,
    },
    /// `Sub` variant.
    Sub {
        flags: IntArithFlags,
        lhs: ValueRef,
        rhs: ValueRef,
    },
    /// `Mul` variant.
    Mul {
        flags: IntArithFlags,
        lhs: ValueRef,
        rhs: ValueRef,
    },
    /// `UDiv` variant.
    UDiv {
        exact: bool,
        lhs: ValueRef,
        rhs: ValueRef,
    },
    /// `SDiv` variant.
    SDiv {
        exact: bool,
        lhs: ValueRef,
        rhs: ValueRef,
    },
    /// `URem` variant.
    URem {
        lhs: ValueRef,
        rhs: ValueRef,
    },
    /// `SRem` variant.
    SRem {
        lhs: ValueRef,
        rhs: ValueRef,
    },

    // --- Bitwise ---
    /// `And` variant.
    And {
        lhs: ValueRef,
        rhs: ValueRef,
    },
    /// `Or` variant.
    Or {
        lhs: ValueRef,
        rhs: ValueRef,
    },
    /// `Xor` variant.
    Xor {
        lhs: ValueRef,
        rhs: ValueRef,
    },
    /// `Shl` variant.
    Shl {
        flags: IntArithFlags,
        lhs: ValueRef,
        rhs: ValueRef,
    },
    /// `LShr` variant.
    LShr {
        exact: bool,
        lhs: ValueRef,
        rhs: ValueRef,
    },
    /// `AShr` variant.
    AShr {
        exact: bool,
        lhs: ValueRef,
        rhs: ValueRef,
    },

    // --- Floating-point arithmetic ---
    /// `FAdd` variant.
    FAdd {
        flags: FastMathFlags,
        lhs: ValueRef,
        rhs: ValueRef,
    },
    /// `FSub` variant.
    FSub {
        flags: FastMathFlags,
        lhs: ValueRef,
        rhs: ValueRef,
    },
    /// `FMul` variant.
    FMul {
        flags: FastMathFlags,
        lhs: ValueRef,
        rhs: ValueRef,
    },
    /// `FDiv` variant.
    FDiv {
        flags: FastMathFlags,
        lhs: ValueRef,
        rhs: ValueRef,
    },
    /// `FRem` variant.
    FRem {
        flags: FastMathFlags,
        lhs: ValueRef,
        rhs: ValueRef,
    },
    /// `FNeg` variant.
    FNeg {
        flags: FastMathFlags,
        operand: ValueRef,
    },

    // --- Comparisons ---
    /// `ICmp` variant.
    ICmp {
        pred: IntPredicate,
        lhs: ValueRef,
        rhs: ValueRef,
    },
    /// `FCmp` variant.
    FCmp {
        flags: FastMathFlags,
        pred: FloatPredicate,
        lhs: ValueRef,
        rhs: ValueRef,
    },

    // --- Memory ---
    /// `Alloca` variant.
    Alloca {
        alloc_ty: TypeId,
        num_elements: Option<ValueRef>,
        align: Option<u32>,
    },
    /// `Load` variant.
    Load {
        ty: TypeId,
        ptr: ValueRef,
        align: Option<u32>,
        volatile: bool,
    },
    /// `Store` variant.
    Store {
        val: ValueRef,
        ptr: ValueRef,
        align: Option<u32>,
        volatile: bool,
    },
    /// `GetElementPtr` variant.
    GetElementPtr {
        inbounds: bool,
        base_ty: TypeId,
        ptr: ValueRef,
        indices: Vec<ValueRef>,
    },

    // --- Casts ---
    /// `Trunc` variant.
    Trunc {
        val: ValueRef,
        to: TypeId,
    },
    /// `ZExt` variant.
    ZExt {
        val: ValueRef,
        to: TypeId,
    },
    /// `SExt` variant.
    SExt {
        val: ValueRef,
        to: TypeId,
    },
    /// `FPTrunc` variant.
    FPTrunc {
        val: ValueRef,
        to: TypeId,
    },
    /// `FPExt` variant.
    FPExt {
        val: ValueRef,
        to: TypeId,
    },
    /// `FPToUI` variant.
    FPToUI {
        val: ValueRef,
        to: TypeId,
    },
    /// `FPToSI` variant.
    FPToSI {
        val: ValueRef,
        to: TypeId,
    },
    /// `UIToFP` variant.
    UIToFP {
        val: ValueRef,
        to: TypeId,
    },
    /// `SIToFP` variant.
    SIToFP {
        val: ValueRef,
        to: TypeId,
    },
    /// `PtrToInt` variant.
    PtrToInt {
        val: ValueRef,
        to: TypeId,
    },
    /// `IntToPtr` variant.
    IntToPtr {
        val: ValueRef,
        to: TypeId,
    },
    /// `BitCast` variant.
    BitCast {
        val: ValueRef,
        to: TypeId,
    },
    /// `AddrSpaceCast` variant.
    AddrSpaceCast {
        val: ValueRef,
        to: TypeId,
    },
    /// `Freeze` variant.
    Freeze {
        val: ValueRef,
    },

    // --- Misc ---
    /// `Select` variant.
    Select {
        cond: ValueRef,
        then_val: ValueRef,
        else_val: ValueRef,
    },
    /// `Phi` variant.
    Phi {
        ty: TypeId,
        incoming: Vec<(ValueRef, BlockId)>,
    },
    /// `ExtractValue` variant.
    ExtractValue {
        aggregate: ValueRef,
        indices: Vec<u32>,
    },
    /// `InsertValue` variant.
    InsertValue {
        aggregate: ValueRef,
        val: ValueRef,
        indices: Vec<u32>,
    },
    /// `ExtractElement` variant.
    ExtractElement {
        vec: ValueRef,
        idx: ValueRef,
    },
    /// `InsertElement` variant.
    InsertElement {
        vec: ValueRef,
        val: ValueRef,
        idx: ValueRef,
    },
    /// `ShuffleVector` variant.
    ShuffleVector {
        v1: ValueRef,
        v2: ValueRef,
        mask: Vec<i32>,
    },

    // --- Call ---
    /// `Call` variant.
    Call {
        tail: TailCallKind,
        callee_ty: TypeId,
        callee: ValueRef,
        args: Vec<ValueRef>,
    },
    /// Inline assembly call-site passthrough.
    ///
    /// This models LLVM IR `call <ty> asm [sideeffect] [alignstack]
    /// "<template>", "<constraints>"(<args>)`. Constraint solving and
    /// output-register assignment are intentionally deferred; backends may only
    /// support side-effect-only passthrough templates at first.
    InlineAsm {
        asm_string: String,
        constraints: String,
        side_effect: bool,
        align_stack: bool,
        args: Vec<ValueRef>,
    },

    // --- Atomics (issue #205) ---
    /// Standalone memory fence.  Result type is `void`.
    Fence {
        /// Memory ordering.
        ordering: MemOrdering,
    },
    /// Atomic compare-and-exchange.
    ///
    /// Result type is the struct `{ <value-ty>, i1 }` — the old value at
    /// `*ptr` followed by a boolean success flag.  Backends are expected to
    /// emit LOCK CMPXCHG on x86 or LDXR/STXR (or LSE CASAL) on AArch64.
    CmpXchg {
        /// Pointer operand.
        ptr: ValueRef,
        /// Expected old value.
        cmp: ValueRef,
        /// Replacement value.
        new_val: ValueRef,
        /// Ordering on success.
        success_ord: MemOrdering,
        /// Ordering on failure (must not be stronger than `success_ord`).
        fail_ord: MemOrdering,
        /// `true` for `cmpxchg weak` — the implementation may spuriously fail.
        weak: bool,
        /// `volatile` flag (no optimization across this op).
        volatile: bool,
    },
    /// Atomic read-modify-write.  Result type is the same as the value type.
    AtomicRmw {
        /// Operation kind (xchg/add/and/...).
        op: RmwOp,
        /// Pointer operand.
        ptr: ValueRef,
        /// Right-hand side / new value depending on `op`.
        val: ValueRef,
        /// Memory ordering.
        ordering: MemOrdering,
        /// `volatile` flag.
        volatile: bool,
    },

    // --- Terminators ---
    /// `Ret` variant.
    Ret {
        val: Option<ValueRef>,
    },
    /// `Br` variant.
    Br {
        dest: BlockId,
    },
    /// `CondBr` variant.
    CondBr {
        cond: ValueRef,
        then_dest: BlockId,
        else_dest: BlockId,
    },
    /// `Switch` variant.
    Switch {
        val: ValueRef,
        default: BlockId,
        cases: Vec<(ValueRef, BlockId)>,
    },
    /// `Unreachable` variant.
    Unreachable,
}

impl InstrKind {
    /// Public API for `is_terminator`.
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
            InstrKind::Freeze { .. } => "freeze",
            InstrKind::Select { .. } => "select",
            InstrKind::Phi { .. } => "phi",
            InstrKind::ExtractValue { .. } => "extractvalue",
            InstrKind::InsertValue { .. } => "insertvalue",
            InstrKind::ExtractElement { .. } => "extractelement",
            InstrKind::InsertElement { .. } => "insertelement",
            InstrKind::ShuffleVector { .. } => "shufflevector",
            InstrKind::Call { .. } => "call",
            InstrKind::InlineAsm { .. } => "call asm",
            InstrKind::Fence { .. } => "fence",
            InstrKind::CmpXchg { .. } => "cmpxchg",
            InstrKind::AtomicRmw { .. } => "atomicrmw",
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
            | InstrKind::AddrSpaceCast { val, .. }
            | InstrKind::Freeze { val } => vec![*val],

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
            InstrKind::InlineAsm { args, .. } => args.clone(),
            InstrKind::Fence { .. } => vec![],
            InstrKind::CmpXchg {
                ptr, cmp, new_val, ..
            } => vec![*ptr, *cmp, *new_val],
            InstrKind::AtomicRmw { ptr, val, .. } => vec![*ptr, *val],
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
            | InstrKind::Freeze { .. }
            | InstrKind::Select { .. }
            | InstrKind::Phi { .. }
            | InstrKind::ExtractValue { .. }
            | InstrKind::InsertValue { .. }
            | InstrKind::ExtractElement { .. }
            | InstrKind::InsertElement { .. }
            | InstrKind::ShuffleVector { .. }
            | InstrKind::Call { .. }
            | InstrKind::InlineAsm { .. }
            | InstrKind::Fence { .. }
            | InstrKind::CmpXchg { .. }
            | InstrKind::AtomicRmw { .. } => vec![],
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
    /// Public API for `kind`.
    pub kind: InstrKind,
}

impl Instruction {
    /// Public API for `new`.
    pub fn new(name: Option<String>, ty: TypeId, kind: InstrKind) -> Self {
        Instruction { name, ty, kind }
    }

    /// Public API for `is_terminator`.
    pub fn is_terminator(&self) -> bool {
        self.kind.is_terminator()
    }

    /// Public API for `operands`.
    pub fn operands(&self) -> Vec<ValueRef> {
        self.kind.operands()
    }

    /// Public API for `successors`.
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

    // ── operands() — atomics (#205) ──────────────────────────────────────────

    #[test]
    fn operands_fence_has_no_value_operands() {
        let k = InstrKind::Fence { ordering: MemOrdering::SeqCst };
        assert_eq!(k.operands(), vec![]);
    }

    #[test]
    fn operands_cmpxchg_returns_ptr_cmp_new() {
        let k = InstrKind::CmpXchg {
            ptr: c0(),
            cmp: c1(),
            new_val: c2(),
            success_ord: MemOrdering::AcqRel,
            fail_ord: MemOrdering::Acquire,
            weak: false,
            volatile: false,
        };
        assert_eq!(k.operands(), vec![c0(), c1(), c2()]);
    }

    #[test]
    fn operands_atomicrmw_returns_ptr_val() {
        let k = InstrKind::AtomicRmw {
            op: RmwOp::Add,
            ptr: c0(),
            val: c1(),
            ordering: MemOrdering::SeqCst,
            volatile: false,
        };
        assert_eq!(k.operands(), vec![c0(), c1()]);
    }

    // ── successors() — atomics are not terminators ───────────────────────────

    #[test]
    fn successors_atomics_are_empty() {
        let fence = InstrKind::Fence { ordering: MemOrdering::SeqCst };
        let cas = InstrKind::CmpXchg {
            ptr: c0(),
            cmp: c1(),
            new_val: c2(),
            success_ord: MemOrdering::SeqCst,
            fail_ord: MemOrdering::Monotonic,
            weak: true,
            volatile: false,
        };
        let rmw = InstrKind::AtomicRmw {
            op: RmwOp::Xchg,
            ptr: c0(),
            val: c1(),
            ordering: MemOrdering::Acquire,
            volatile: false,
        };
        assert_eq!(fence.successors(), vec![]);
        assert_eq!(cas.successors(), vec![]);
        assert_eq!(rmw.successors(), vec![]);
        assert!(!fence.is_terminator());
        assert!(!cas.is_terminator());
        assert!(!rmw.is_terminator());
    }

    #[test]
    fn opcode_names_atomics() {
        let fence = InstrKind::Fence { ordering: MemOrdering::SeqCst };
        let cas = InstrKind::CmpXchg {
            ptr: c0(),
            cmp: c1(),
            new_val: c2(),
            success_ord: MemOrdering::SeqCst,
            fail_ord: MemOrdering::Monotonic,
            weak: false,
            volatile: false,
        };
        let rmw = InstrKind::AtomicRmw {
            op: RmwOp::Add,
            ptr: c0(),
            val: c1(),
            ordering: MemOrdering::SeqCst,
            volatile: false,
        };
        assert_eq!(fence.opcode(), "fence");
        assert_eq!(cas.opcode(), "cmpxchg");
        assert_eq!(rmw.opcode(), "atomicrmw");
    }

    #[test]
    fn mem_ordering_as_str_matches_ir_syntax() {
        assert_eq!(MemOrdering::Unordered.as_str(), "unordered");
        assert_eq!(MemOrdering::Monotonic.as_str(), "monotonic");
        assert_eq!(MemOrdering::Acquire.as_str(), "acquire");
        assert_eq!(MemOrdering::Release.as_str(), "release");
        assert_eq!(MemOrdering::AcqRel.as_str(), "acq_rel");
        assert_eq!(MemOrdering::SeqCst.as_str(), "seq_cst");
    }

    #[test]
    fn rmw_op_as_str_matches_ir_syntax() {
        assert_eq!(RmwOp::Xchg.as_str(), "xchg");
        assert_eq!(RmwOp::Add.as_str(), "add");
        assert_eq!(RmwOp::Sub.as_str(), "sub");
        assert_eq!(RmwOp::Nand.as_str(), "nand");
        assert_eq!(RmwOp::UMax.as_str(), "umax");
        assert_eq!(RmwOp::FAdd.as_str(), "fadd");
    }
}
