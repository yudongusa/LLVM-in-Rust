//! Differential fuzzing harness for LLVM-in-Rust.
//!
//! Generates seed-deterministic random IR programs, compiles them through the
//! JIT execution engine, and compares results to expected values computed by
//! direct IR interpretation.

pub mod gen;
pub mod harness;

pub use gen::FuzzGen;
pub use harness::{run_campaign, run_campaign_from, run_one, DiffResult};
