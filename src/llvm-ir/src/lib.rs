//! Core IR types: types, values, instructions, basic blocks, functions, and modules.

pub mod basic_block;
/// Public API for `builder`.
pub mod builder;
/// Public API for `context`.
pub mod context;
/// Public API for `function`.
pub mod function;
/// Public API for `instruction`.
pub mod instruction;
/// Public API for `module`.
pub mod module;
/// Public API for `printer`.
pub mod printer;
/// Public API for `types`.
pub mod types;
/// Public API for `value`.
pub mod value;

// Re-export key types at crate root for ergonomic use.
/// Public API for `re-export`.
pub use basic_block::BasicBlock;
/// Public API for `re-export`.
pub use builder::Builder;
/// Public API for `re-export`.
pub use context::{
    ArgId, BlockId, ConstId, Context, FunctionId, GlobalId, InstrId, TypeId, ValueRef,
};
/// Public API for `re-export`.
pub use function::Function;
/// Public API for `re-export`.
pub use instruction::{
    ExactFlag, FastMathFlags, FloatPredicate, InstrKind, Instruction, IntArithFlags, IntPredicate,
    TailCallKind,
};
/// Public API for `re-export`.
pub use module::{DebugLocation, Module};
/// Public API for `re-export`.
pub use printer::Printer;
/// Public API for `re-export`.
pub use types::{FloatKind, FunctionType, StructType, TypeData};
/// Public API for `re-export`.
pub use value::{Argument, ConstantData, GlobalVariable, Linkage};
