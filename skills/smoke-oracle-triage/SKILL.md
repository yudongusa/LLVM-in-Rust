---
name: smoke-oracle-triage
description: Implement issue #102 by quarantining known smoke-oracle mismatches, preserving CI signal, and documenting follow-up root-cause work.
---

# Smoke Oracle Triage

Use this skill for issue #102 and similar temporary CI-unblock quarantines.

## Workflow

1. Confirm failing smoke cases from CI logs.
2. Quarantine only known failing tests (narrowest scope, OS-gated if needed).
3. Annotate each ignored test with tracking issue context.
4. Re-run smoke-related tests and full suite.
5. Post PR review feedback, then monitor CI checks and iterate until green.

## Validation

```bash
cargo +stable test -p llvm-ir-parser --test smoke
cargo +stable test -q
```

## Notes

- Keep quarantine minimal and explicit.
- Never ignore broad modules when case-level ignores are sufficient.
- Use `gh pr checks` + `gh run view --log-failed` loop before merge.
