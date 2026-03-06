//! Optimization passes: mem2reg, DCE, constant folding/propagation, and inlining.

pub mod const_prop;
pub mod constant_fold;
pub mod dce;
pub mod dead_arg_elim;
pub mod gvn;
pub mod inline_pass;
pub mod ipcp;
pub mod loop_unroll;
pub mod mem2reg;
pub mod pass;
pub mod pipeline;
mod value_rewrite;

pub use const_prop::ConstProp;
pub use constant_fold::try_fold;
pub use dce::DeadCodeElim;
pub use dead_arg_elim::DeadArgElim;
pub use gvn::Gvn;
pub use inline_pass::Inliner;
pub use ipcp::Ipcp;
pub use loop_unroll::LoopUnroll;
pub use mem2reg::Mem2Reg;
pub use pass::{FunctionPass, ModulePass, PassManager};
pub use pipeline::{build_pipeline, OptLevel};
