#[cfg(test)]
mod tests {
    use llvm_ir::Module;
    use llvm_ir_parser::parser::parse;
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
}
