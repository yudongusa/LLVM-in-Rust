//! Top-level crate re-exporting all pipeline stages.

pub use llvm_analysis as analysis;
pub use llvm_bitcode as bitcode;
pub use llvm_codegen as codegen;
pub use llvm_ir as ir;
pub use llvm_ir_parser as ir_parser;
pub use llvm_target_arm as target_arm;
pub use llvm_target_x86 as target_x86;
pub use llvm_transforms as transforms;
