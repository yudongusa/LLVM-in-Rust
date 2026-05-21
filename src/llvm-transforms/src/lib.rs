//! Optimization passes: mem2reg, DCE, constant folding/propagation, and inlining.
//!
//! # Examples
//!
//! Run the O2 optimization pipeline on a simple module:
//!
//! ```no_run
//! use llvm_ir::{Builder, Context, Linkage, Module};
//! use llvm_transforms::{build_pipeline, OptLevel};
//! let mut ctx = Context::new();
//! let mut module = Module::new("demo");
//! let mut b = Builder::new(&mut ctx, &mut module);
//! let i32_ty = b.ctx.i32_ty;
//! b.add_function("f", i32_ty, vec![], vec![], false, Linkage::External);
//! let entry = b.add_block("entry");
//! b.position_at_end(entry);
//! let c = b.const_int(i32_ty, 42);
//! b.build_ret(c);
//! drop(b);
//! let mut pm = build_pipeline(OptLevel::O2);
//! pm.run_until_fixed_point(&mut ctx, &mut module, 8);
//! assert_eq!(module.functions.len(), 1);
//! ```

/// Public API for `asan`.
pub mod asan;
/// Public API for `cfg_simplify`.
pub mod cfg_simplify;
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
/// Public API for `licm`.
pub mod licm;
/// Public API for `loop_unroll`.
pub mod loop_unroll;
/// Public API for `jump_threading`.
pub mod jump_threading;
/// Public API for `mem2reg`.
pub mod mem2reg;
/// Public API for `pass`.
pub mod pass;
/// Public API for `pipeline`.
pub mod pipeline;
/// Public API for `slp`.
pub mod slp;
/// Public API for `sroa`.
pub mod sroa;
/// Public API for `tailcall`.
pub mod tailcall;
mod value_rewrite;

/// Public API for `re-export`.
pub use asan::Asan;
/// Public API for `re-export`.
pub use cfg_simplify::CfgSimplify;
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
pub use licm::Licm;
/// Public API for `re-export`.
pub use loop_unroll::LoopUnroll;
/// Public API for `re-export`.
pub use jump_threading::JumpThreading;
/// Public API for `re-export`.
pub use mem2reg::Mem2Reg;
/// Public API for `re-export`.
pub use pass::{FunctionPass, ModulePass, PassManager};
/// Public API for `re-export`.
pub use pipeline::{build_pipeline, OptLevel};
/// Public API for `re-export`.
pub use slp::SlpVectorizer;
/// Public API for `re-export`.
pub use sroa::Sroa;
/// Public API for `re-export`.
pub use tailcall::TailCallOpt;
