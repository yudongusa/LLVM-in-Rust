//! Standard optimization pipelines (`-O0` through `-O3`).
//!
//! These presets provide a stable public API for frontends/examples to avoid
//! manually assembling pass sequences.

use crate::{pass::PassManager, ConstProp, DeadCodeElim, Inliner, Mem2Reg};

/// Optimization level preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptLevel {
    O0,
    O1,
    O2,
    O3,
}

impl OptLevel {
    /// Parse command-line style strings such as `"O2"` or `"2"`.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "o0" | "0" => Some(Self::O0),
            "o1" | "1" => Some(Self::O1),
            "o2" | "2" => Some(Self::O2),
            "o3" | "3" => Some(Self::O3),
            _ => None,
        }
    }
}

/// Build a pass pipeline for the requested optimization level.
///
/// Current implementation uses passes available in this repository today.
/// Future O2/O3-only passes (GVN/unroll/vectorize/IPA) can be added in-place
/// without breaking the public API.
pub fn build_pipeline(level: OptLevel) -> PassManager {
    let mut pm = PassManager::new();

    match level {
        OptLevel::O0 => {
            // Intentionally empty.
        }
        OptLevel::O1 => {
            pm.add_function_pass(Mem2Reg);
            pm.add_function_pass(ConstProp);
            pm.add_function_pass(DeadCodeElim);
        }
        OptLevel::O2 => {
            pm.add_function_pass(Mem2Reg);
            pm.add_module_pass(Inliner::default());
            pm.add_function_pass(ConstProp);
            pm.add_function_pass(DeadCodeElim);
            // Clean up after inlining.
            pm.add_function_pass(ConstProp);
            pm.add_function_pass(DeadCodeElim);
        }
        OptLevel::O3 => {
            pm.add_function_pass(Mem2Reg);
            pm.add_module_pass(Inliner { size_limit: 100 });
            pm.add_function_pass(ConstProp);
            pm.add_function_pass(DeadCodeElim);
            // Extra cleanup rounds as a placeholder for future aggressive O3.
            pm.add_function_pass(ConstProp);
            pm.add_function_pass(DeadCodeElim);
            pm.add_function_pass(ConstProp);
            pm.add_function_pass(DeadCodeElim);
        }
    }

    pm
}

#[cfg(test)]
mod tests {
    use super::*;
    use llvm_ir::{Builder, Context, InstrKind, Linkage, Module, ValueRef};

    fn make_dead_code_fn() -> (Context, Module) {
        let mut ctx = Context::new();
        let mut module = Module::new("test");
        let mut b = Builder::new(&mut ctx, &mut module);
        b.add_function("main", b.ctx.i32_ty, vec![], vec![], false, Linkage::External);
        let entry = b.add_block("entry");
        b.position_at_end(entry);

        let c1 = b.const_int(b.ctx.i32_ty, 1);
        let c2 = b.const_int(b.ctx.i32_ty, 2);
        let c100 = b.const_int(b.ctx.i32_ty, 100);
        let c7 = b.const_int(b.ctx.i32_ty, 7);

        let dead1 = b.build_add("dead1", c1, c2);
        let _dead2 = b.build_mul("dead2", dead1, c100);
        b.build_ret(c7);

        (ctx, module)
    }

    #[test]
    fn o2_preserves_return_semantics_and_reduces_body_size_vs_o0() {
        let (mut ctx_o0, mut m_o0) = make_dead_code_fn();
        let (mut ctx_o2, mut m_o2) = make_dead_code_fn();

        let mut pm_o0 = build_pipeline(OptLevel::O0);
        let mut pm_o2 = build_pipeline(OptLevel::O2);
        pm_o0.run_until_fixed_point(&mut ctx_o0, &mut m_o0, 3);
        pm_o2.run_until_fixed_point(&mut ctx_o2, &mut m_o2, 8);

        let f0 = &m_o0.functions[0];
        let f2 = &m_o2.functions[0];

        let o0_body_len = f0.blocks[0].body.len();
        let o2_body_len = f2.blocks[0].body.len();
        assert!(
            o2_body_len < o0_body_len,
            "O2 should reduce instruction count (o0={}, o2={})",
            o0_body_len,
            o2_body_len
        );

        let t0 = f0.blocks[0].terminator.expect("o0 terminator");
        let t2 = f2.blocks[0].terminator.expect("o2 terminator");

        match (&f0.instr(t0).kind, &f2.instr(t2).kind) {
            (InstrKind::Ret { val: Some(v0) }, InstrKind::Ret { val: Some(v2) }) => {
                assert_eq!(
                    *v0,
                    ValueRef::Constant(ctx_o0.const_int(ctx_o0.i32_ty, 7)),
                    "o0 should still return constant 7"
                );
                assert_eq!(
                    *v2,
                    ValueRef::Constant(ctx_o2.const_int(ctx_o2.i32_ty, 7)),
                    "o2 should return constant 7"
                );
            }
            _ => panic!("expected both pipelines to end with ret i32 7"),
        }
    }

    #[test]
    fn parse_opt_level_variants() {
        assert_eq!(OptLevel::parse("O0"), Some(OptLevel::O0));
        assert_eq!(OptLevel::parse("1"), Some(OptLevel::O1));
        assert_eq!(OptLevel::parse("o2"), Some(OptLevel::O2));
        assert_eq!(OptLevel::parse(" 3 "), Some(OptLevel::O3));
        assert_eq!(OptLevel::parse("Ox"), None);
    }
}
