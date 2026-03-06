#[cfg(test)]
mod tests {
    use llvm_ir::{Builder, GlobalId, Linkage, Module, ValueRef};
    use llvm_ir_parser::parser::parse;
    use llvm_ir::InstrKind;
    use llvm_transforms::{build_pipeline, pass::PassManager, OptLevel};

    const FIXTURE: &str = include_str!("../fixtures/sample.ll");

    fn instruction_count(module: &Module) -> usize {
        module
            .functions
            .iter()
            .map(|f| {
                f.blocks
                    .iter()
                    .map(|bb| bb.body.len() + usize::from(bb.terminator.is_some()))
                    .sum::<usize>()
            })
            .sum()
    }

    fn optimized_instruction_count(level: OptLevel) -> usize {
        let (mut ctx, mut module) = parse(FIXTURE).expect("sample.ll should parse");
        let mut pm: PassManager = build_pipeline(level);
        pm.run_until_fixed_point(&mut ctx, &mut module, 8);
        instruction_count(&module)
    }

    fn call_arg_pressure(module: &Module) -> usize {
        module
            .functions
            .iter()
            .flat_map(|f| &f.instructions)
            .map(|i| match &i.kind {
                InstrKind::Call { args, .. } => args.len(),
                _ => 0,
            })
            .sum()
    }

    fn build_ipa_fixture() -> (llvm_ir::Context, Module) {
        let mut ctx = llvm_ir::Context::new();
        let mut module = Module::new("ipa");
        let mut b = Builder::new(&mut ctx, &mut module);
        let i64_ty = b.ctx.i64_ty;
        let worker_ty = b
            .ctx
            .mk_fn_type(i64_ty, vec![i64_ty, i64_ty, i64_ty, i64_ty], false);

        b.add_function(
            "worker",
            i64_ty,
            vec![i64_ty, i64_ty, i64_ty, i64_ty],
            vec!["x".into(), "y".into(), "dead1".into(), "dead2".into()],
            false,
            Linkage::External,
        );
        let w_entry = b.add_block("worker.entry");
        b.position_at_end(w_entry);
        let x = b.get_arg(0);
        let y = b.get_arg(1);
        let a = b.build_add("a", x, y);
        b.build_ret(a);

        b.add_function(
            "driver",
            i64_ty,
            vec![i64_ty],
            vec!["x".into()],
            false,
            Linkage::External,
        );
        let d_entry = b.add_block("driver.entry");
        b.position_at_end(d_entry);
        let x0 = b.get_arg(0);
        let c7 = b.const_int(i64_ty, 7);
        let c100 = b.const_int(i64_ty, 100);
        let c200 = b.const_int(i64_ty, 200);
        let c101 = b.const_int(i64_ty, 101);
        let c201 = b.const_int(i64_ty, 201);
        let c102 = b.const_int(i64_ty, 102);
        let c202 = b.const_int(i64_ty, 202);
        let c1 = b.build_call(
            "c1",
            i64_ty,
            worker_ty,
            ValueRef::Global(GlobalId(0)),
            vec![x0, c7, c100, c200],
        );
        let c2 = b.build_call(
            "c2",
            i64_ty,
            worker_ty,
            ValueRef::Global(GlobalId(0)),
            vec![c1, c7, c101, c201],
        );
        let c3 = b.build_call(
            "c3",
            i64_ty,
            worker_ty,
            ValueRef::Global(GlobalId(0)),
            vec![c2, c7, c102, c202],
        );
        b.build_ret(c3);
        (ctx, module)
    }

    fn optimized_call_arg_pressure_from_builder(level: OptLevel) -> usize {
        let (mut ctx, mut module) = build_ipa_fixture();
        let mut pm: PassManager = build_pipeline(level);
        pm.run_until_fixed_point(&mut ctx, &mut module, 8);
        call_arg_pressure(&module)
    }

    #[test]
    fn sample_ll_o2_reduces_ir_instruction_count_by_at_least_10_percent_vs_o1() {
        let o1 = optimized_instruction_count(OptLevel::O1);
        let o2 = optimized_instruction_count(OptLevel::O2);
        assert!(
            o2 * 10 <= o1 * 9,
            "expected >=10% reduction on sample.ll (o1={}, o2={})",
            o1,
            o2
        );
    }

    #[test]
    fn ipa_fixture_o3_reduces_call_arg_pressure_vs_o2() {
        let o2 = optimized_call_arg_pressure_from_builder(OptLevel::O2);
        let o3 = optimized_call_arg_pressure_from_builder(OptLevel::O3);
        assert!(
            o3 < o2,
            "expected O3 IPA passes to reduce call-arg pressure (o2={}, o3={})",
            o2,
            o3
        );
    }
}
