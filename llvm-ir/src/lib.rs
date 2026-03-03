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
pub use context::{
    Context, TypeId, FunctionId, BlockId, InstrId, ArgId, ConstId, GlobalId, ValueRef,
};
pub use types::{TypeData, FloatKind, StructType, FunctionType};
pub use value::{ConstantData, Argument, GlobalVariable, Linkage};
pub use instruction::{
    Instruction, InstrKind, IntArithFlags, FastMathFlags, ExactFlag,
    IntPredicate, FloatPredicate, TailCallKind,
};
pub use basic_block::BasicBlock;
pub use function::Function;
pub use module::Module;
pub use builder::Builder;
pub use printer::Printer;
