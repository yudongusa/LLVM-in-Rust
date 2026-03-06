---
name: simd-vectorization
description: Implement issue #86 end-to-end by adding x86 vector lowering with scalar fallback at O0, SSE4.2/AVX2 gated emission, and vector end-to-end validation.
---

# SIMD Vectorization

Use this skill to execute issue #86 in incremental phases with correctness-first behavior.

## Workflow

1. Baseline scalar correctness for vector IR at O0.
2. Add target feature gating API (`TargetFeatures`) in x86 backend.
3. Implement SSE4.2 lowering for common vector patterns.
4. Extend to AVX2 where supported.
5. Add vector end-to-end test (`.ll -> object -> link -> run`).

## Step 1: Scalar Baseline

- Ensure `ExtractElement`, `InsertElement`, and `ShuffleVector` do not panic.
- For unsupported vector ops, lower to conservative scalar fallback or deterministic zero/stub behavior with explicit comments.
- Add tests proving lowering is non-panicking for every vector `InstrKind` variant.

## Step 2: Feature Gating

- Introduce x86 `TargetFeatures` with at least `sse42` and `avx2` flags.
- Default behavior should be conservative (disabled unless explicitly enabled).
- Gate SIMD instruction emission on features; fallback path must remain correct.

## Step 3: SSE4.2 First

- Implement 128-bit lowering for most common integer and float vector arithmetic.
- Add encoding tests per opcode family in `llvm-target-x86/src/encode.rs` tests.
- Keep mapping explicit; avoid silent fallback to unrelated opcodes.

## Step 4: AVX2 Follow-up

- Extend lowering to 256-bit forms for the same opcode families.
- Reuse feature-gated selector logic; avoid duplicating correctness code.

## Step 5: Validation

Run at minimum:

```bash
cargo +stable test -p llvm-target-x86
cargo +stable test -p llvm-ir-parser
cargo +stable test
```

If native link/run smoke cannot execute in environment, document exact blocker and include parser/codegen-only evidence.

## Step 6: Review + Full Test

- Review implementation PR with focus on codegen correctness, gating behavior, and fallback safety.
- Run targeted checks and full suite (`cargo +stable test`) unless blocked.

## Step 7: Issue+Fix Loop (Same PR)

- If concrete problems are found, open GitHub issue(s) documenting each.
- Fix findings in the same PR branch and push follow-up commits.

## Step 8: Post Review Summary

- Post PR review feedback (`gh pr review --comment` or `gh pr comment`) summarizing findings and fixes.
- Include issue links in the PR review summary.

## Resources

- Use [`references/issue-86-plan.md`](references/issue-86-plan.md) for acceptance checklist and sequencing.
- Use [`scripts/vector_opcode_audit.sh`](scripts/vector_opcode_audit.sh) to scan vector-op coverage and TODO sites.
