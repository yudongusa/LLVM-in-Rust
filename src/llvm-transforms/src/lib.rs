//! Optimization passes: mem2reg, DCE, constant folding/propagation, and inlining.

pub mod const_prop;
/// Public API for `constant_fold`.
pub mod constant_fold;
/// Public API for `constant_fold_pass`.
pub mod constant_fold_pass;
/// Public API for `dce`.
pub mod dce;
/// Public API for `dead_arg_elim`.
pub mod dead_arg_elim;
/// Public API for `gvn`.
pub mod gvn;
/// Public API for `inline_pass`.
pub mod inline_pass;
/// Public API for `ipcp`.
pub mod ipcp;
/// Public API for `loop_unroll`.
pub mod loop_unroll;
/// Public API for `mem2reg`.
pub mod mem2reg;
/// Public API for `pass`.
pub mod pass;
/// Public API for `pipeline`.
pub mod pipeline;
mod value_rewrite;

/// Public API for `re-export`.
pub use const_prop::ConstProp;
/// Public API for `re-export`.
pub use constant_fold::try_fold;
/// Public API for `re-export`.
pub use constant_fold_pass::ConstantFold;
/// Public API for `re-export`.
pub use dce::DeadCodeElim;
/// Public API for `re-export`.
pub use dead_arg_elim::DeadArgElim;
/// Public API for `re-export`.
pub use gvn::Gvn;
/// Public API for `re-export`.
pub use inline_pass::Inliner;
/// Public API for `re-export`.
pub use ipcp::Ipcp;
/// Public API for `re-export`.
pub use loop_unroll::LoopUnroll;
/// Public API for `re-export`.
pub use mem2reg::Mem2Reg;
/// Public API for `re-export`.
pub use pass::{FunctionPass, ModulePass, PassManager};
/// Public API for `re-export`.
pub use pipeline::{build_pipeline, OptLevel};
