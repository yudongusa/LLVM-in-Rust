//! Proof-of-concept rustc codegen backend for LLVM-in-Rust.
//!
//! # Current state
//!
//! A full `rustc_codegen_ssa::traits::CodegenBackend` implementation requires
//! `#![feature(rustc_private)]` and the `rustc-dev` toolchain component, neither
//! of which is available on the stable channel.  This crate therefore ships in two
//! modes:
//!
//! * **Stable (default)** — no `rustc-backend` feature.  Provides shim tests that
//!   exercise the same IR pipeline a real backend would invoke (parse → optimise →
//!   emit), proving the pipeline is correct without requiring nightly toolchains.
//!   See the `shim` module and the gap analysis in `docs/rustc-backend-gap-analysis.md`.
//!
//! * **Nightly** — compile with `--features rustc-backend`.  Wires the pipeline to
//!   the real `rustc_codegen_ssa` traits (not yet fully implemented; see TODO items
//!   in `codegen_backend.rs`).
//!
//! # Architecture overview
//!
//! ```text
//! rustc MIR  ──►  MIR→IR translation  ──►  llvm-ir Module
//!                                               │
//!                                        llvm-transforms
//!                                               │
//!                                        llvm-codegen (isel + regalloc)
//!                                               │
//!                                     llvm-target-x86 / llvm-target-arm
//!                                               │
//!                                          ObjectFile  ──►  rustc linker
//! ```
//!
//! The translation layer (MIR → llvm-ir) is the largest missing piece; all other
//! stages are already implemented by the sibling crates.

pub mod aggregate;
pub mod drop_glue;
pub mod place;
pub mod shim;
pub mod unwind;

#[cfg(feature = "rustc-backend")]
pub mod codegen_backend;

/// The name reported to rustc when this backend is loaded as a plugin.
pub const BACKEND_NAME: &str = "llvm-in-rust";
