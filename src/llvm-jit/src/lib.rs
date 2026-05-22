//! JIT ExecutionEngine for LLVM-in-Rust.
//!
//! Compiles an IR module to native x86-64 machine code, maps executable
//! memory pages via `mmap`, and returns callable function pointers.
//!
//! # Safety model
//! - [`SimpleJit::get_function_ptr`] returns a raw `*const u8`; the caller is
//!   responsible for casting and calling it as `unsafe`.
//! - [`SimpleJit::get_fn`] is `unsafe`: the caller must guarantee that the
//!   type parameter `F` matches the compiled function's ABI and signature.

use llvm_codegen::{
    emit_object,
    isel::IselBackend,
    regalloc::{allocate_registers, apply_allocation, compute_live_intervals, insert_spill_reloads,
               RegAllocStrategy},
    ObjectFormat,
};
use llvm_ir::{Context, Module};
use llvm_target_x86::{
    instructions::{MOV_LOAD_MR, MOV_STORE_RM},
    TargetFeatures, X86Backend, X86Emitter,
};
use llvm_transforms::{build_pipeline, OptLevel};
use std::collections::HashMap;

// ── error type ─────────────────────────────────────────────────────────────

/// Errors produced by the JIT engine.
#[derive(Debug)]
pub enum JitError {
    /// The compilation pipeline failed with an explanatory message.
    CompilationFailed(String),
    /// `mmap` returned `MAP_FAILED`.
    AllocationFailed,
}

impl std::fmt::Display for JitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JitError::CompilationFailed(msg) => write!(f, "JIT compilation failed: {msg}"),
            JitError::AllocationFailed => write!(f, "JIT memory allocation failed (mmap)"),
        }
    }
}

impl std::error::Error for JitError {}

// ── ExecutionEngine trait ───────────────────────────────────────────────────

/// Trait for JIT execution engines.
pub trait ExecutionEngine {
    /// Compile all non-declaration functions in `module` and make them
    /// available via [`get_function_ptr`](Self::get_function_ptr).
    ///
    /// The engine is permitted to mutate `ctx` and `module` in place
    /// (e.g. to run optimisation passes before code generation).
    fn add_module(&mut self, ctx: &mut Context, module: &mut Module) -> Result<(), JitError>;

    /// Return a raw pointer to the compiled function with the given name,
    /// or `None` if the name is not known.
    fn get_function_ptr(&self, name: &str) -> Option<*const u8>;
}

// ── mmap page allocation helper ─────────────────────────────────────────────

/// A single `mmap`-allocated executable memory region.
struct ExecPage {
    /// Base address returned by `mmap`.
    base: *mut u8,
    /// Total size of the mapping in bytes (rounded up to a whole page).
    size: usize,
}

// SAFETY: The raw pointer is a private mapping that is only accessible via
// immutable shared references after `mprotect` makes it read+exec; no
// mutable aliasing can occur.
unsafe impl Send for ExecPage {}
unsafe impl Sync for ExecPage {}

impl ExecPage {
    /// Allocate `needed` bytes of executable memory.
    ///
    /// The allocation is initially `PROT_READ | PROT_WRITE` so we can copy
    /// bytes into it; call [`Self::make_exec`] afterwards.
    fn new(needed: usize) -> Result<Self, JitError> {
        // SAFETY: sysconf is a pure read of a system constant.
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) as usize };
        let size = (needed + page_size - 1) & !(page_size - 1);

        // SAFETY: NULL addr lets the kernel choose the base; MAP_PRIVATE|MAP_ANON
        // means no file backing; -1 fd and offset 0 are required for MAP_ANON.
        let base = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANON,
                -1,
                0,
            )
        };

        if base == libc::MAP_FAILED {
            return Err(JitError::AllocationFailed);
        }

        Ok(Self {
            base: base as *mut u8,
            size,
        })
    }

    /// Copy `bytes` into the page starting at `offset`.
    ///
    /// # Panics
    /// Panics if `offset + bytes.len() > self.size`.
    fn copy_from(&mut self, offset: usize, bytes: &[u8]) {
        assert!(
            offset + bytes.len() <= self.size,
            "copy_from: overflow (offset={} len={} size={})",
            offset,
            bytes.len(),
            self.size
        );
        // SAFETY: We verified the range is within our owned mapping, which is
        // currently writable (PROT_READ | PROT_WRITE).
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), self.base.add(offset), bytes.len());
        }
    }

    /// Switch the page to `PROT_READ | PROT_EXEC` (removes write permission).
    fn make_exec(&self) -> Result<(), JitError> {
        // SAFETY: self.base and self.size describe the exact mapping we created.
        let rc = unsafe { libc::mprotect(self.base as *mut libc::c_void, self.size, libc::PROT_READ | libc::PROT_EXEC) };
        if rc != 0 {
            Err(JitError::AllocationFailed)
        } else {
            Ok(())
        }
    }

    /// Return a pointer `offset` bytes past the page base.
    fn ptr_at(&self, offset: usize) -> *const u8 {
        // SAFETY: offset was validated by copy_from; the pointer is within the mapping.
        unsafe { self.base.add(offset) }
    }
}

impl Drop for ExecPage {
    fn drop(&mut self) {
        // SAFETY: self.base and self.size are exactly what was returned by mmap.
        unsafe {
            libc::munmap(self.base as *mut libc::c_void, self.size);
        }
    }
}

// ── SimpleJit ───────────────────────────────────────────────────────────────

/// A simple mmap-based JIT execution engine targeting x86-64.
///
/// Call [`add_module`](Self::add_module) to compile an IR module, then
/// [`get_function_ptr`](Self::get_function_ptr) or the safe-ish wrapper
/// [`get_fn`](Self::get_fn) to obtain callable pointers.
pub struct SimpleJit {
    /// Executable memory pages allocated by this engine.
    pages: Vec<ExecPage>,
    /// Map from function name → absolute pointer into one of `pages`.
    symbols: HashMap<String, *const u8>,
}

// SAFETY: ExecPage is Send+Sync (see above); raw pointers in symbols are
// only created from ExecPage::ptr_at after make_exec, so they are stable
// and immutable.
unsafe impl Send for SimpleJit {}
unsafe impl Sync for SimpleJit {}

impl SimpleJit {
    /// Create an empty JIT engine.
    pub fn new() -> Self {
        Self {
            pages: Vec::new(),
            symbols: HashMap::new(),
        }
    }

    /// Return a safe-ish wrapper that transmutes the stored pointer to `F`.
    ///
    /// # Safety
    /// The caller must guarantee that `F` exactly matches the compiled
    /// function's calling convention, argument types, and return type.
    pub unsafe fn get_fn<F: Copy>(&self, name: &str) -> Option<F> {
        let ptr = self.get_function_ptr(name)?;
        // SAFETY: caller guarantees F matches the compiled ABI.
        Some(unsafe { std::mem::transmute_copy(&ptr) })
    }
}

impl Default for SimpleJit {
    fn default() -> Self {
        Self::new()
    }
}

impl ExecutionEngine for SimpleJit {
    fn add_module(&mut self, ctx: &mut Context, module: &mut Module) -> Result<(), JitError> {
        // Run the O2 optimisation pipeline in-place.
        let mut pm = build_pipeline(OptLevel::O2);
        pm.run_until_fixed_point(ctx, module, 8);

        // Collect non-declaration functions.
        let non_decls: Vec<usize> = module
            .functions
            .iter()
            .enumerate()
            .filter(|(_, f)| !f.is_declaration)
            .map(|(i, _)| i)
            .collect();

        // Nothing to do for a declarations-only module.
        if non_decls.is_empty() {
            return Ok(());
        }

        // Determine the host's native object format (we only need the text bytes).
        let fmt = if cfg!(target_os = "linux") {
            ObjectFormat::Elf
        } else if cfg!(target_os = "windows") {
            ObjectFormat::Coff
        } else {
            ObjectFormat::MachO
        };

        // Compile each function and collect (name, bytes) pairs.
        let mut function_codes: Vec<(String, Vec<u8>)> = Vec::new();

        for func_idx in non_decls {
            let func = &module.functions[func_idx];
            let func_name = func.name.clone();

            let mut backend = X86Backend::new(TargetFeatures::baseline());
            let mut mf = backend.lower_function(ctx, module, func);

            let intervals = compute_live_intervals(&mf);
            let mut result =
                allocate_registers(&intervals, &mf.allocatable_pregs, &mf.allocatable_fp_pregs, RegAllocStrategy::LinearScan);
            insert_spill_reloads(&mut mf, &mut result, MOV_LOAD_MR, MOV_STORE_RM, MOV_LOAD_MR, MOV_STORE_RM);
            apply_allocation(&mut mf, &result);

            let mut emitter = X86Emitter::new(fmt);
            let obj = emit_object(&mf, &mut emitter);

            // Extract the .text / __text section bytes.
            let text_bytes = obj
                .sections
                .into_iter()
                .find(|s| s.name == ".text" || s.name == "__text")
                .ok_or_else(|| {
                    JitError::CompilationFailed(format!(
                        "no text section produced for function '{}'",
                        func_name
                    ))
                })?
                .data;

            if text_bytes.is_empty() {
                return Err(JitError::CompilationFailed(format!(
                    "function '{}' produced empty text section",
                    func_name
                )));
            }

            function_codes.push((func_name, text_bytes));
        }

        // Lay out all functions into a single executable page.
        let total_size: usize = function_codes.iter().map(|(_, b)| b.len()).sum();

        let mut page = ExecPage::new(total_size)?;

        let mut offset = 0usize;
        for (name, bytes) in &function_codes {
            page.copy_from(offset, bytes);
            let ptr = page.ptr_at(offset);
            self.symbols.insert(name.clone(), ptr);
            offset += bytes.len();
        }

        page.make_exec()?;
        self.pages.push(page);

        Ok(())
    }

    fn get_function_ptr(&self, name: &str) -> Option<*const u8> {
        self.symbols.get(name).copied()
    }
}

// ── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use llvm_ir_parser::parser::parse;

    // Execution tests only run where we emit native x86-64 code.
    // On other architectures we still compile to bytes but skip execution.

    /// Compile a module and return the JIT engine.
    fn jit_from_src(src: &str) -> SimpleJit {
        let (mut ctx, mut module) = parse(src).expect("parse failed");
        let mut jit = SimpleJit::new();
        jit.add_module(&mut ctx, &mut module).expect("add_module failed");
        jit
    }

    // ── compile-to-bytes test (runs everywhere) ─────────────────────────────

    #[test]
    fn compile_to_bytes_non_empty() {
        let src = r#"
define i32 @answer() {
entry:
  ret i32 42
}
"#;
        let (mut ctx, mut module) = parse(src).expect("parse failed");
        let mut jit = SimpleJit::new();
        jit.add_module(&mut ctx, &mut module).expect("add_module failed");
        // At least one symbol must have been registered.
        assert!(jit.get_function_ptr("answer").is_some());
    }

    // ── execution tests (x86-64 only) ───────────────────────────────────────

    #[test]
    #[cfg(target_arch = "x86_64")]
    fn jit_return_constant() {
        let src = r#"
define i32 @answer() {
entry:
  ret i32 42
}
"#;
        let jit = jit_from_src(src);
        let ptr = jit.get_function_ptr("answer").expect("symbol not found");
        // SAFETY: ptr points to compiled x86-64 code with signature `fn() -> i32`.
        let result: i32 = unsafe { std::mem::transmute::<*const u8, fn() -> i32>(ptr)() };
        assert_eq!(result, 42);
    }

    #[test]
    #[cfg(target_arch = "x86_64")]
    fn jit_add_two_ints() {
        let src = r#"
define i32 @add(i32 %a, i32 %b) {
entry:
  %r = add i32 %a, %b
  ret i32 %r
}
"#;
        let jit = jit_from_src(src);
        let ptr = jit.get_function_ptr("add").expect("symbol not found");
        // SAFETY: ptr points to compiled x86-64 code with signature `fn(i32, i32) -> i32`.
        let result: i32 =
            unsafe { std::mem::transmute::<*const u8, fn(i32, i32) -> i32>(ptr)(3, 4) };
        assert_eq!(result, 7);
    }

    #[test]
    #[cfg(target_arch = "x86_64")]
    fn jit_fibonacci() {
        // Explicit SSA phi nodes — no alloca/mem2reg dependency.
        let src = r#"
define i32 @fib(i32 %n) {
entry:
  %cmp = icmp sle i32 %n, 1
  br i1 %cmp, label %base_case, label %loop_init

base_case:
  ret i32 %n

loop_init:
  br label %loop_body

loop_body:
  %a = phi i32 [ 0, %loop_init ], [ %b, %loop_body ]
  %b = phi i32 [ 1, %loop_init ], [ %next, %loop_body ]
  %i = phi i32 [ 1, %loop_init ], [ %i1, %loop_body ]
  %next = add i32 %a, %b
  %i1 = add i32 %i, 1
  %cond = icmp slt i32 %i1, %n
  br i1 %cond, label %loop_body, label %loop_exit

loop_exit:
  ret i32 %next
}
"#;
        let jit = jit_from_src(src);
        let ptr = jit.get_function_ptr("fib").expect("symbol not found");
        // SAFETY: ptr points to compiled x86-64 code with signature `fn(i32) -> i32`.
        let f = unsafe { std::mem::transmute::<*const u8, fn(i32) -> i32>(ptr) };
        assert_eq!(unsafe { f(10) }, 55);
        assert_eq!(unsafe { f(0) }, 0);
        assert_eq!(unsafe { f(1) }, 1);
    }

    #[test]
    #[cfg(target_arch = "x86_64")]
    fn jit_multiple_functions() {
        let src = r#"
define i32 @double(i32 %x) {
entry:
  %r = mul i32 %x, 2
  ret i32 %r
}
define i32 @triple(i32 %x) {
entry:
  %r = mul i32 %x, 3
  ret i32 %r
}
"#;
        let jit = jit_from_src(src);

        let p_double = jit.get_function_ptr("double").expect("double not found");
        let p_triple = jit.get_function_ptr("triple").expect("triple not found");

        // SAFETY: both functions have signature `fn(i32) -> i32`.
        let double = unsafe { std::mem::transmute::<*const u8, fn(i32) -> i32>(p_double) };
        let triple = unsafe { std::mem::transmute::<*const u8, fn(i32) -> i32>(p_triple) };

        assert_eq!(unsafe { double(5) }, 10);
        assert_eq!(unsafe { triple(5) }, 15);
    }

    #[test]
    fn jit_unknown_symbol_returns_none() {
        let src = r#"
define i32 @answer() {
entry:
  ret i32 1
}
"#;
        let jit = jit_from_src(src);
        assert!(jit.get_function_ptr("nonexistent").is_none());
    }

    #[test]
    fn jit_rejects_empty_module() {
        // A declarations-only module: add_module should succeed but register no
        // symbols, because there is nothing to compile.
        let src = r#"
declare i32 @printf(i8*, ...)
"#;
        let (mut ctx, mut module) = parse(src).expect("parse failed");
        let mut jit = SimpleJit::new();
        jit.add_module(&mut ctx, &mut module).expect("add_module should succeed");
        assert!(jit.get_function_ptr("printf").is_none());
    }
}
