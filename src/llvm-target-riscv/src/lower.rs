//! Minimal RV64 lowering scaffold.

use crate::instructions::RET;
use crate::regs::{ALLOCATABLE, CALLEE_SAVED};
use llvm_codegen::isel::{IselBackend, MInstr, MachineFunction};
use llvm_ir::{Context, Function, InstrKind, Module};

/// RISC-V instruction-selection backend scaffold.
#[derive(Default)]
pub struct RiscVBackend;

impl IselBackend for RiscVBackend {
    fn lower_function(
        &mut self,
        _ctx: &Context,
        _module: &Module,
        func: &Function,
    ) -> MachineFunction {
        let mut mf = MachineFunction::new(func.name.clone());
        mf.allocatable_pregs = ALLOCATABLE.to_vec();
        mf.callee_saved_pregs = CALLEE_SAVED.to_vec();

        if func.is_declaration || func.blocks.is_empty() {
            return mf;
        }

        for (bi, bb) in func.blocks.iter().enumerate() {
            let label = if bi == 0 {
                func.name.clone()
            } else {
                format!("{}.{}", func.name, bb.name)
            };
            mf.add_block(label);
        }

        // Keep scaffold behavior explicit: unsupported instructions/terminators
        // fail fast rather than silently emitting incorrect machine code.
        for (bi, bb) in func.blocks.iter().enumerate() {
            for &iid in &bb.body {
                let instr = func.instr(iid);
                panic!(
                    "RISC-V lowering does not yet support body instruction {:?} in '{}'",
                    instr.kind, func.name
                );
            }
            if let Some(tid) = bb.terminator {
                match &func.instr(tid).kind {
                    InstrKind::Ret { val: None } => mf.push(bi, MInstr::new(RET)),
                    InstrKind::Ret { val: Some(_), .. } => {
                        panic!(
                            "RISC-V lowering does not yet support returning values in '{}'",
                            func.name
                        );
                    }
                    other => {
                        panic!(
                            "RISC-V lowering does not yet support terminator {:?} in '{}'",
                            other, func.name
                        );
                    }
                }
            }
        }

        mf
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use llvm_ir::{Builder, Context, Linkage, Module};

    #[test]
    fn lower_declaration_is_empty() {
        let mut ctx = Context::new();
        let mut module = Module::new("m");
        let mut b = Builder::new(&mut ctx, &mut module);
        b.add_function("decl", b.ctx.i64_ty, vec![], vec![], true, Linkage::External);
        let f = &module.functions[0];
        let mut be = RiscVBackend;
        let mf = be.lower_function(&ctx, &module, f);
        assert!(mf.blocks.is_empty());
    }

    #[test]
    fn lower_non_declaration_creates_blocks() {
        let mut ctx = Context::new();
        let mut module = Module::new("m");
        let mut b = Builder::new(&mut ctx, &mut module);
        b.add_function("f", b.ctx.void_ty, vec![], vec![], false, Linkage::External);
        let entry = b.add_block("entry");
        b.position_at_end(entry);
        b.build_ret_void();

        let f = &module.functions[0];
        let mut be = RiscVBackend;
        let mf = be.lower_function(&ctx, &module, f);
        assert_eq!(mf.blocks.len(), 1);
        assert!(!mf.blocks[0].instrs.is_empty());
    }

    #[test]
    #[should_panic(expected = "does not yet support returning values")]
    fn lower_ret_value_panics() {
        let mut ctx = Context::new();
        let mut module = Module::new("m");
        let mut b = Builder::new(&mut ctx, &mut module);
        b.add_function("f", b.ctx.i64_ty, vec![], vec![], false, Linkage::External);
        let entry = b.add_block("entry");
        b.position_at_end(entry);
        let c0 = b.const_i64(0);
        b.build_ret(c0);
        let f = &module.functions[0];
        let mut be = RiscVBackend;
        let _ = be.lower_function(&ctx, &module, f);
    }

    #[test]
    #[should_panic(expected = "does not yet support body instruction")]
    fn lower_body_instruction_panics() {
        let mut ctx = Context::new();
        let mut module = Module::new("m");
        let mut b = Builder::new(&mut ctx, &mut module);
        b.add_function("f", b.ctx.void_ty, vec![], vec![], false, Linkage::External);
        let entry = b.add_block("entry");
        b.position_at_end(entry);
        let c1 = b.const_i64(1);
        let c2 = b.const_i64(2);
        let _ = b.build_add("sum", c1, c2);
        b.build_ret_void();
        let f = &module.functions[0];
        let mut be = RiscVBackend;
        let _ = be.lower_function(&ctx, &module, f);
    }
}
