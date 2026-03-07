//! Core IR types: types, values, instructions, basic blocks, functions, and modules.

pub mod basic_block;
pub mod builder;
pub mod context;
pub mod function;
pub mod instruction;
pub mod module;
pub mod printer;
pub mod types;
pub mod value;

// Re-export key types at crate root for ergonomic use.
pub use basic_block::BasicBlock;
pub use builder::Builder;
pub use context::{
    ArgId, BlockId, ConstId, Context, FunctionId, GlobalId, InstrId, TypeId, ValueRef,
};
pub use function::Function;
pub use instruction::{
    ExactFlag, FastMathFlags, FloatPredicate, InstrKind, Instruction, IntArithFlags, IntPredicate,
    TailCallKind,
};
pub use module::{DebugLocation, Module};
pub use printer::Printer;
pub use types::{FloatKind, FunctionType, StructType, TypeData};
pub use value::{Argument, ConstantData, GlobalVariable, Linkage};
