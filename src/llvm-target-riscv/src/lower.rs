//! Minimal RV64 lowering scaffold.

use crate::instructions::{ADDI, NOP, RET};
use crate::regs::{ALLOCATABLE, CALLEE_SAVED, RET_REG};
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

        // Keep current scaffold deterministic: emit one placeholder per IR instr
        // and lower function returns explicitly into `a0` where possible.
        for (bi, bb) in func.blocks.iter().enumerate() {
            for &iid in &bb.body {
                match &func.instr(iid).kind {
                    InstrKind::Ret { val: Some(_), .. } => {
                        let v = mf.fresh_vreg();
                        // Placeholder "move immediate 0" into vreg; later passes can replace.
                        mf.push(bi, MInstr::new(ADDI).with_dst(v).with_preg(RET_REG).with_imm(0));
                        mf.push(bi, MInstr::new(RET));
                    }
                    _ => mf.push(bi, MInstr::new(NOP)),
                }
            }
            if let Some(tid) = bb.terminator {
                match &func.instr(tid).kind {
                    InstrKind::Ret { .. } => mf.push(bi, MInstr::new(RET)),
                    _ => mf.push(bi, MInstr::new(NOP)),
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
        b.add_function("f", b.ctx.i64_ty, vec![], vec![], false, Linkage::External);
        let entry = b.add_block("entry");
        b.position_at_end(entry);
        let c0 = b.const_i64(0);
        b.build_ret(c0);

        let f = &module.functions[0];
        let mut be = RiscVBackend;
        let mf = be.lower_function(&ctx, &module, f);
        assert_eq!(mf.blocks.len(), 1);
        assert!(!mf.blocks[0].instrs.is_empty());
    }
}
