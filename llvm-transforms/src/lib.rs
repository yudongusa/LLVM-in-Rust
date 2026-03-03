//! Optimization passes: mem2reg, DCE, constant folding, and inlining.

pub mod constant_fold;
pub mod dce;
pub mod inline_pass;
pub mod mem2reg;
pub mod pass;
