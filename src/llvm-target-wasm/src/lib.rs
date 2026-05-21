//! WebAssembly (wasm32) target backend for LLVM-in-Rust.
//!
//! # Strategy
//!
//! This backend uses the **"all-locals"** strategy: every SSA value (function
//! argument or instruction result) is mapped to a wasm `local` variable.
//! Assignments are `local.set`, uses are `local.get`.  This entirely avoids
//! the register allocator and produces valid (if unoptimised) wasm.
//!
//! Structured control flow is handled with a simple "wrapping" approach:
//! each basic block gets a wasm `block` construct so that forward branches
//! can be expressed as `br N`.  Back-edges (loop headers) get an additional
//! `loop` wrapper.
//!
//! # Usage
//!
//! ```no_run
//! use llvm_target_wasm::compile_to_wasm;
//! let bytes = compile_to_wasm("define i32 @f() { entry: ret i32 0 }").unwrap();
//! assert_eq!(&bytes[..4], b"\0asm");
//! ```

pub mod encode;
pub mod instructions;
pub mod lower;
pub mod reloop;
pub mod types;

pub use lower::WasmBackend;
pub use reloop::{build_control_tree, should_use_br_table, ControlNode};

use llvm_ir::{Context, Module};
use lower::WasmFunction;

/// Assemble a complete WebAssembly binary from the given `WasmFunction` list
/// and the IR context/module (needed for type-section construction).
pub fn assemble_module(
    ctx: &Context,
    module: &Module,
    wasm_functions: &[WasmFunction],
) -> Vec<u8> {
    encode::assemble_module(ctx, module, wasm_functions)
}

/// Parse `ir`, run optimisations, lower to wasm and return the binary.
///
/// # Errors
/// Returns a human-readable error string on parse or compile failure.
pub fn compile_to_wasm(ir: &str) -> Result<Vec<u8>, String> {
    use llvm_ir_parser::parser::parse;
    use llvm_transforms::{build_pipeline, OptLevel};

    let (mut ctx, mut module) = parse(ir).map_err(|e| format!("parse error: {e}"))?;

    let mut pm = build_pipeline(OptLevel::O2);
    pm.run_until_fixed_point(&mut ctx, &mut module, 8);

    let mut backend = WasmBackend::new();
    let wasm_fns = backend.lower_module(&ctx, &module);

    Ok(assemble_module(&ctx, &module, &wasm_fns))
}

// ── tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // Helper to find a byte sequence within a byte slice.
    fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle)
    }

    // Helper to find a string in a byte slice.
    fn contains_str(haystack: &[u8], s: &str) -> bool {
        contains_bytes(haystack, s.as_bytes())
    }

    #[test]
    fn wasm_magic_bytes() {
        let bytes =
            compile_to_wasm("define i32 @f() { entry: ret i32 0 }").expect("compile failed");
        assert_eq!(&bytes[..4], b"\0asm", "first 4 bytes must be wasm magic");
    }

    #[test]
    fn wasm_version() {
        let bytes =
            compile_to_wasm("define i32 @f() { entry: ret i32 0 }").expect("compile failed");
        assert_eq!(
            &bytes[4..8],
            &[0x01, 0x00, 0x00, 0x00],
            "bytes 4-7 must be wasm version 1"
        );
    }

    #[test]
    fn wasm_return_const() {
        let bytes = compile_to_wasm("define i32 @g() { entry: ret i32 42 }").expect("compile");
        // Must contain i32.const (0x41) somewhere in the code.
        assert!(
            bytes.contains(&0x41),
            "binary must contain i32.const (0x41)"
        );
        // 42 in LEB128 is just 0x2A.
        assert!(bytes.contains(&0x2A), "binary must contain LEB128 of 42 (0x2A)");
    }

    #[test]
    fn wasm_add_two_ints() {
        let ir = "define i32 @add(i32 %a, i32 %b) {\nentry:\n  %r = add i32 %a, %b\n  ret i32 %r\n}";
        let bytes = compile_to_wasm(ir).expect("compile");
        // Must contain i32.add (0x6A).
        assert!(
            bytes.contains(&0x6A),
            "binary must contain i32.add (0x6A)"
        );
    }

    #[test]
    fn wasm_export_name() {
        let ir = "define i32 @my_func() {\nentry:\n  ret i32 0\n}";
        let bytes = compile_to_wasm(ir).expect("compile");
        assert!(
            contains_str(&bytes, "my_func"),
            "binary must contain export name \"my_func\""
        );
    }

    #[test]
    fn wasm_multi_function() {
        let ir = concat!(
            "define i32 @foo() { entry: ret i32 1 }\n",
            "define i32 @bar() { entry: ret i32 2 }\n"
        );
        let bytes = compile_to_wasm(ir).expect("compile");
        assert!(contains_str(&bytes, "foo"), "binary must contain \"foo\"");
        assert!(contains_str(&bytes, "bar"), "binary must contain \"bar\"");
    }

    #[test]
    fn wasm_conditional_branch() {
        let ir = concat!(
            "define i32 @cond(i32 %x) {\n",
            "entry:\n",
            "  %c = icmp eq i32 %x, 0\n",
            "  br i1 %c, label %then, label %else\n",
            "then:\n",
            "  ret i32 1\n",
            "else:\n",
            "  ret i32 2\n",
            "}\n"
        );
        let bytes = compile_to_wasm(ir).expect("compile");
        // Must contain `if` (0x04) or `br_if` (0x0D).
        let has_if = bytes.contains(&0x04);
        let has_br_if = bytes.contains(&0x0D);
        assert!(
            has_if || has_br_if,
            "conditional branch binary must contain if (0x04) or br_if (0x0D)"
        );
    }

    #[test]
    fn wasm_declarations_excluded() {
        let ir = "declare i32 @ext()\n";
        let bytes = compile_to_wasm(ir).expect("compile");
        // Must have valid magic+version.
        assert_eq!(&bytes[..4], b"\0asm");
        assert_eq!(&bytes[4..8], &[0x01, 0x00, 0x00, 0x00]);
        // Should NOT contain "ext" as an export (declaration is not defined).
        // The binary may be just magic+version with no sections.
        // Check that there's no code section entry: code section id is 0x0A.
        // If present, it would have count > 0.  For a declaration-only module,
        // there should be no non-declaration functions, so wasm_fns is empty
        // and no code/function/export sections are emitted.
        assert!(
            !contains_str(&bytes, "ext"),
            "declaration @ext must not appear as an export"
        );
    }
}
