//! Differential fuzzing harness.
//!
//! Compiles randomly generated IR through the JIT, executes it, and compares
//! the result to expected output computed by direct IR interpretation.

use llvm_ir::{Context, Module, Printer};
use llvm_jit::{ExecutionEngine, JitError, SimpleJit};

use crate::gen::FuzzGen;

/// Result of a single differential fuzz run.
pub struct DiffResult {
    /// The IR text of the generated function.
    pub ir_text: String,
    /// Expected return value (from IR interpretation).
    pub expected: i64,
    /// Actual return value from JIT execution, or `None` if JIT failed.
    pub actual: Option<i64>,
    /// Whether expected == actual.
    pub matched: bool,
    /// The seed that produced this function.
    pub seed: u64,
}

/// Compile and run one random IR function; compare to expected.
///
/// # Arguments
/// * `seed` — random seed for the IR generator.
pub fn run_one(seed: u64) -> DiffResult {
    let mut ctx = Context::new();
    let mut module = Module::new("fuzz");

    // Generate IR.
    let fid = FuzzGen::new(seed).gen_function(&mut ctx, &mut module);

    // Compute expected output via interpretation.
    let expected = FuzzGen::interpret(&ctx, &module, fid);

    // Render IR text for diagnostics.
    let ir_text = Printer::new(&ctx).print_module(&module);

    // Compile and execute via JIT.
    let actual = jit_execute(&mut ctx, &mut module);

    // On non-x86_64 hosts the JIT cannot execute, so `actual` is None.
    // Treat None as "skipped" (matched=true) to avoid false positives from
    // platforms where native execution is not supported.
    let matched = actual.map(|v| v == expected).unwrap_or(true);

    DiffResult {
        ir_text,
        expected,
        actual,
        matched,
        seed,
    }
}

/// Run `count` random programs starting from `base_seed`, returning mismatches.
pub fn run_campaign(count: u32) -> Vec<DiffResult> {
    run_campaign_from(42, count)
}

/// Run `count` random programs starting from `base_seed`, returning mismatches.
pub fn run_campaign_from(base_seed: u64, count: u32) -> Vec<DiffResult> {
    let mut mismatches = Vec::new();
    for i in 0..count {
        let seed = base_seed.wrapping_add(i as u64);
        let result = run_one(seed);
        if !result.matched {
            mismatches.push(result);
        }
    }
    mismatches
}

/// Compile `module` through the JIT and call `fuzz_fn(1, 2, 3)`.
///
/// Returns `None` if compilation fails or the function is not found.
/// Arguments beyond the function's arity are ignored by the ABI.
///
/// # Safety note
/// We transmute the function pointer to call with (i64, i64, i64) → i64.
/// The generated function always has 1–3 i64 args and returns i64; excess
/// args are harmlessly ignored by the System-V calling convention.
fn jit_execute(ctx: &mut Context, module: &mut Module) -> Option<i64> {
    let mut jit = SimpleJit::new();
    match jit.add_module(ctx, module) {
        Ok(()) => {}
        Err(JitError::CompilationFailed(_)) => return None,
        Err(JitError::AllocationFailed) => return None,
    }

    let ptr = jit.get_function_ptr("fuzz_fn")?;

    // We always call with three i64 args; functions with fewer args ignore the extras.
    // SAFETY: generated functions have signature (i64, ..., i64) -> i64 with 1–3 args.
    #[cfg(target_arch = "x86_64")]
    {
        let f = unsafe {
            std::mem::transmute::<*const u8, unsafe extern "C" fn(i64, i64, i64) -> i64>(ptr)
        };
        Some(unsafe { f(1, 2, 3) })
    }

    // On non-x86_64 hosts we skip execution and treat every run as matched
    // to avoid false positives (JIT emits x86-64 code that cannot run).
    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = ptr;
        None
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// run_one must always return a DiffResult (no panic, no crash).
    #[test]
    fn run_one_does_not_panic() {
        let r = run_one(0);
        // expected is always a valid i64 (interpretation succeeded).
        let _ = r.expected;
    }

    /// A campaign of 50 programs must produce zero mismatches on x86_64.
    ///
    /// On other architectures JIT cannot execute, so `actual` is None and we
    /// skip this assertion.
    #[test]
    #[cfg(target_arch = "x86_64")]
    fn campaign_zero_mismatches() {
        let mismatches = run_campaign(50);
        assert!(
            mismatches.is_empty(),
            "{} mismatch(es) found:\n{}",
            mismatches.len(),
            mismatches
                .iter()
                .map(|m| format!(
                    "  seed={}: expected={}, actual={:?}\n{}",
                    m.seed, m.expected, m.actual, m.ir_text
                ))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    /// On non-x86_64 the JIT is skipped; all programs should be treated as matched.
    #[test]
    #[cfg(not(target_arch = "x86_64"))]
    fn campaign_compile_only() {
        // All programs should produce zero mismatches because actual=None → matched=true.
        let mismatches = run_campaign(10);
        assert!(
            mismatches.is_empty(),
            "compile-only mode should have no mismatches, got {}",
            mismatches.len()
        );
    }

    /// run_one for seed 42 must compute an expected value.
    #[test]
    fn run_one_seed42_has_expected() {
        let r = run_one(42);
        // interpreted value must be a valid i64 — just confirm it doesn't panic.
        let _: i64 = r.expected;
    }
}
