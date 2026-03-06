---
name: rust-stable-compat
description: Migrate this Rust workspace from nightly-only requirements to stable-compatible development and CI. Use when removing `#![feature(...)]`, replacing unstable benchmark harness usage, validating `cargo +stable build/test`, and updating docs/CI for issue #90.
---

# Rust Stable Compat

Use this skill to execute issue #90 end-to-end with low regression risk.

## Workflow

1. Run baseline audit.
2. Remove nightly-only code paths.
3. Preserve benchmark intent on stable Rust.
4. Update CI and docs.
5. Run stable validation commands and report gaps.

## Step 1: Baseline Audit

- Run `scripts/stable_audit.sh`.
- Confirm exact nightly-only usage sites.
- Record whether nightly dependency is only benchmark-related.

## Step 2: Remove Nightly-Only Features

- Delete `#![feature(...)]` usage.
- Replace `extern crate test` + `#[bench]` with stable-compatible benchmarking (prefer Criterion).
- Keep benchmark cases equivalent so historical comparisons stay meaningful.

## Step 3: Keep Bench UX Practical

- Ensure contributors can run benches with one command.
- Prefer `cargo bench -p llvm-bench` behavior that works on stable.
- Keep benchmark file structure straightforward (`criterion_group!`, `criterion_main!`).

## Step 4: Update Integration Points

- Update crate manifests in benchmark crate.
- Update CI workflow to include stable checks.
- Update README/docs: prerequisites and benchmark commands must match the new flow.

## Step 5: Validate

Run at minimum:

```bash
cargo +stable build --all-targets
cargo +stable test
cargo +stable bench -p llvm-bench --no-run
```

If any command cannot run in the environment, document exactly what was blocked and why.

## PR Checklist

- No `#![feature(` remains in workspace.
- Stable build and tests pass.
- Bench target compiles on stable.
- CI/doc updates are included in the same PR.

## Resources

- Use [`references/issue-90-plan.md`](references/issue-90-plan.md) for acceptance criteria and execution order.
- Use [`scripts/stable_audit.sh`](scripts/stable_audit.sh) for quick nightly-feature detection and toolchain sanity checks.
