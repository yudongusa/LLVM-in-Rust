---
name: ipa-optimizer
description: Implement issue #87 by adding inter-procedural analysis primitives (CallGraph + SCC), IPCP/dead-argument module passes, and O3 integration with benchmark validation.
---

# IPA Optimizer

Use this skill to execute issue #87 with analysis-first, regression-safe steps.

## Workflow

1. Build `CallGraph` in `llvm-analysis` (direct/indirect/external edges + SCCs).
2. Add an IPCP module pass with at least one constant-argument specialization.
3. Add dead-argument elimination module pass and tests.
4. Integrate IPA passes into O3 pipeline.
5. Validate measurable O3 improvement on a multi-function benchmark fixture.

## Step 1: CallGraph Core

- Add a module-level call graph data type.
- Preserve edge kind (`direct`, `indirect`, `external`) for downstream decisions.
- Provide caller/callee queries and SCC traversal API.

## Step 2: IPCP

- Detect callsites with stable constant arguments.
- Clone/specialize callee where profitable.
- Re-run intra-procedural const propagation on specialized clone.
- Keep linkage and function-id bookkeeping correct.

## Step 3: Dead Argument Elimination

- Remove parameters unused by callee and all callsites.
- Rewrite function signature and update all affected call instructions.
- Add a targeted regression test.

## Step 4: O3 Integration

- Integrate new module passes into O3 only, unless data supports earlier insertion.
- Ensure fixed-point loop in pass manager converges without oscillation.

## Step 5: Validation

Run at minimum:

```bash
cargo +stable test -p llvm-analysis
cargo +stable test -p llvm-transforms
cargo +stable test
```

If benchmark environment is constrained, capture instruction-count deltas as deterministic proxy.

## Step 6: Review + Full Test

- Review implementation PR for semantic correctness, pass-pipeline safety, and missing tests.
- Run targeted checks and full suite (`cargo +stable test`) unless blocked.

## Step 7: Issue+Fix Loop (Same PR)

- If concrete problems are found, open GitHub issue(s) for traceability.
- Fix them in the same PR branch and push follow-up commits.

## Step 8: Post Review Summary

- Post PR review feedback (`gh pr review --comment` or `gh pr comment`) summarizing findings and fixes.
- Include links to issues opened during review.

## Resources

- Use [`references/issue-87-plan.md`](references/issue-87-plan.md) for acceptance checklist.
- Use [`scripts/callgraph_audit.sh`](scripts/callgraph_audit.sh) to inspect call instructions and edge coverage quickly.
