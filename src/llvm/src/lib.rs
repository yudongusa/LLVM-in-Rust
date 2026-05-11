//! Top-level crate re-exporting all pipeline stages.

pub use llvm_analysis as analysis;
/// Public API for `re-export`.
pub use llvm_bitcode as bitcode;
/// Public API for `re-export`.
pub use llvm_codegen as codegen;
/// Public API for `re-export`.
pub use llvm_ir as ir;
/// Public API for `re-export`.
pub use llvm_ir_parser as ir_parser;
/// Public API for `re-export`.
pub use llvm_target_arm as target_arm;
/// Public API for `re-export`.
pub use llvm_target_riscv as target_riscv;
/// Public API for `re-export`.
pub use llvm_target_x86 as target_x86;
/// Public API for `re-export`.
pub use llvm_transforms as transforms;

/// Public API for `lto`.
pub mod lto;
