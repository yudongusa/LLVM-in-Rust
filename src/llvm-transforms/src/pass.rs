//! Pass infrastructure: FunctionPass, ModulePass, and PassManager.
//!
//! A `FunctionPass` transforms a single `Function` in isolation.
//! A `ModulePass` transforms the whole `Module` (needed for inter-procedural
//! passes such as inlining).
//!
//! `PassManager` sequences module passes and runs them to a fixed point.

use llvm_ir::{Context, Function, Module};

// ---------------------------------------------------------------------------
// Traits
// ---------------------------------------------------------------------------

/// A pass that transforms a single function.
pub trait FunctionPass {
    /// Apply this pass to `func`. Returns `true` if the IR was modified.
    fn run_on_function(&mut self, ctx: &mut Context, func: &mut Function) -> bool;

    /// Human-readable name used in diagnostics.
    fn name(&self) -> &'static str;
}

/// A pass that transforms an entire module.
pub trait ModulePass {
    /// Apply this pass to `module`. Returns `true` if the IR was modified.
    fn run_on_module(&mut self, ctx: &mut Context, module: &mut Module) -> bool;

    /// Human-readable name used in diagnostics.
    fn name(&self) -> &'static str;
}

// ---------------------------------------------------------------------------
// FunctionPassAdapter: lifts a FunctionPass into a ModulePass
// ---------------------------------------------------------------------------

/// Applies a `FunctionPass` to every non-declaration function in the module.
pub struct FunctionPassAdapter<P: FunctionPass> {
    pub pass: P,
}

impl<P: FunctionPass> ModulePass for FunctionPassAdapter<P> {
    fn run_on_module(&mut self, ctx: &mut Context, module: &mut Module) -> bool {
        let mut changed = false;
        for i in 0..module.functions.len() {
            if !module.functions[i].is_declaration {
                changed |= self.pass.run_on_function(ctx, &mut module.functions[i]);
            }
        }
        changed
    }

    fn name(&self) -> &'static str {
        self.pass.name()
    }
}

// ---------------------------------------------------------------------------
// PassManager
// ---------------------------------------------------------------------------

/// Sequences module passes and runs them once, or to a fixed point.
pub struct PassManager {
    passes: Vec<Box<dyn ModulePass>>,
}

impl PassManager {
    pub fn new() -> Self {
        PassManager { passes: Vec::new() }
    }

    /// Add a module-level pass.
    pub fn add_module_pass(&mut self, pass: impl ModulePass + 'static) {
        self.passes.push(Box::new(pass));
    }

    /// Add a function-level pass (wrapped automatically in a `FunctionPassAdapter`).
    pub fn add_function_pass(&mut self, pass: impl FunctionPass + 'static) {
        self.passes.push(Box::new(FunctionPassAdapter { pass }));
    }

    /// Run all passes over `module` once, in order.
    ///
    /// Returns `true` if any pass modified the IR.
    pub fn run(&mut self, ctx: &mut Context, module: &mut Module) -> bool {
        let mut changed = false;
        for pass in &mut self.passes {
            changed |= pass.run_on_module(ctx, module);
        }
        changed
    }

    /// Run all passes repeatedly until the IR stabilises or `max_iter` is reached.
    pub fn run_until_fixed_point(
        &mut self,
        ctx: &mut Context,
        module: &mut Module,
        max_iter: usize,
    ) {
        for _ in 0..max_iter {
            if !self.run(ctx, module) {
                break;
            }
        }
    }
}

impl Default for PassManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use llvm_ir::{BasicBlock, Function, InstrKind, Instruction, Linkage, Module};

    struct NoOpPass;
    impl FunctionPass for NoOpPass {
        fn name(&self) -> &'static str {
            "noop"
        }
        fn run_on_function(&mut self, _ctx: &mut Context, _func: &mut Function) -> bool {
            false
        }
    }

    fn make_one_func_module(ctx: &mut Context) -> Module {
        let fn_ty = ctx.mk_fn_type(ctx.void_ty, vec![], false);
        let mut func = Function::new("f", fn_ty, vec![], Linkage::External);
        let mut bb = BasicBlock::new("entry");
        let iid = func.alloc_instr(Instruction {
            name: None,
            ty: ctx.void_ty,
            kind: InstrKind::Unreachable,
        });
        bb.set_terminator(iid);
        func.add_block(bb);
        let mut m = Module::new("test");
        m.add_function(func);
        m
    }

    #[test]
    fn noop_returns_false() {
        let mut ctx = Context::new();
        let mut module = make_one_func_module(&mut ctx);
        let mut pm = PassManager::new();
        pm.add_function_pass(NoOpPass);
        assert!(!pm.run(&mut ctx, &mut module));
    }

    #[test]
    fn fixed_point_stops_when_stable() {
        let mut ctx = Context::new();
        let mut module = make_one_func_module(&mut ctx);
        let mut pm = PassManager::new();
        pm.add_function_pass(NoOpPass);
        pm.run_until_fixed_point(&mut ctx, &mut module, 100);
    }

    #[test]
    fn declaration_is_skipped() {
        let mut ctx = Context::new();
        let fn_ty = ctx.mk_fn_type(ctx.void_ty, vec![], false);
        let decl = Function::new_declaration("ext", fn_ty, vec![], Linkage::External);
        let mut module = Module::new("test");
        module.add_function(decl);

        struct PanicPass;
        impl FunctionPass for PanicPass {
            fn name(&self) -> &'static str {
                "panic"
            }
            fn run_on_function(&mut self, _: &mut Context, _: &mut Function) -> bool {
                panic!("must not run on a declaration");
            }
        }
        let mut pm = PassManager::new();
        pm.add_function_pass(PanicPass);
        pm.run(&mut ctx, &mut module); // must not panic
    }
}
