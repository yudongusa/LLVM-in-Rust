//! Core IR types: types, values, instructions, basic blocks, functions, and modules.
//!
//! # Examples
//!
//! Build a simple `add` function and verify it has one block:
//!
//! ```no_run
//! use llvm_ir::{Builder, Context, Linkage, Module};
//! let mut ctx = Context::new();
//! let mut module = Module::new("my_module");
//! let mut b = Builder::new(&mut ctx, &mut module);
//! let i32_ty = b.ctx.i32_ty;
//! b.add_function(
//!     "add",
//!     i32_ty,
//!     vec![i32_ty, i32_ty],
//!     vec!["a".into(), "b".into()],
//!     false,
//!     Linkage::External,
//! );
//! let entry = b.add_block("entry");
//! b.position_at_end(entry);
//! let a = b.get_arg(0);
//! let bv = b.get_arg(1);
//! let sum = b.build_add("sum", a, bv);
//! b.build_ret(sum);
//! drop(b);
//! assert_eq!(module.functions[0].blocks.len(), 1);
//! ```

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
    InstrprofIntrinsic, LandingPadClause, MemOrdering, RmwOp, TailCallKind, VpIntrinsic,
};
/// Public API for `re-export`.
pub use module::{DebugLocation, Module};
/// Public API for `re-export`.
pub use printer::Printer;
/// Public API for `re-export`.
pub use types::{FloatKind, FunctionType, StructType, TypeData};
/// Public API for `re-export`.
pub use value::{Argument, ConstExprOp, ConstantData, GlobalVariable, Linkage};
