//! Optimization passes: mem2reg, DCE, constant folding/propagation, and inlining.

pub mod constant_fold;
pub mod const_prop;
pub mod dce;
pub mod inline_pass;
pub mod mem2reg;
pub mod pass;

pub use constant_fold::try_fold;
pub use const_prop::ConstProp;
pub use dce::DeadCodeElim;
pub use inline_pass::Inliner;
pub use mem2reg::Mem2Reg;
pub use pass::{FunctionPass, ModulePass, PassManager};
