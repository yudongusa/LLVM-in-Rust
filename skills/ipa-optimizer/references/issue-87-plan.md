# Issue #87 Execution Plan

## Acceptance Criteria Mapping

- [ ] `CallGraph` in `llvm-analysis` with cycle/DAG/indirect-call tests.
- [ ] IPCP `ModulePass` with at least one specialization test.
- [ ] Dead-argument elimination `ModulePass` with test.
- [ ] O3 pipeline integration.
- [ ] Benchmark improvement at O3 vs O2 on multi-function module.

## Suggested Implementation Order

1. Land `CallGraph` + SCC support in `llvm-analysis`.
2. Land IPCP in `llvm-transforms` using call graph.
3. Land dead-argument elimination with careful signature rewrite.
4. Integrate passes into O3 pipeline and run fixed-point tests.
5. Add benchmark/instruction-count comparison for O3 vs O2.

## Review Checklist

- Call graph classifies direct vs indirect/external calls correctly.
- SCC order is bottom-up and deterministic.
- Function cloning preserves return type and argument ordering.
- Callsite rewrites preserve SSA uses and type correctness.
