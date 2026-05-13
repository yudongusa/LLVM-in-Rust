# Production operations guide

Issue #186 tracks the operator and contributor playbooks for running LLVM-in-Rust in production-like integrations.

## Audience

- **Operators** embedding `llvm` or `llvm-ir-min` in a build/release pipeline.
- **Contributors** triaging parser, optimizer, codegen, performance, or compatibility reports.
- **Release owners** connecting incident response back to RC and rollback decisions.

## Build and validation quick start

Use locked dependencies and the same gates CI runs:

```bash
cargo +stable build --locked --all-targets
cargo +stable test
cargo +stable bench -p llvm-in-rust-bench --no-run
scripts/interoperability_conformance.sh link
scripts/platform_matrix.sh host-core
scripts/release_artifacts.sh dry-run --version ops-smoke
```

Optional deeper checks:

```bash
scripts/compat_stage_a.sh
scripts/compat_stage_b.sh
scripts/sanitizer_matrix.sh miri-core
cargo +stable test -p llvm-in-rust-codegen golden -- --nocapture
```

Capture the command transcript, Git commit, host OS/arch, Rust version, and relevant artifact paths for every production validation run.

## Observability checklist

LLVM-in-Rust is usually embedded as a library or CLI in a larger pipeline. The host pipeline should record:

- input source identity: file name, checksum, producer tool/version, and LLVM IR version when known
- command line or API operation invoked
- target triple/backend/features (`x86_64`, AArch64, RV64GC, SIMD feature gates)
- elapsed time and timeout threshold
- exit status, panic message, stderr, and backtrace when available
- emitted object/archive checksum for reproducibility investigations
- link to CI run, release artifact bundle, or user report

For privacy, minimize and redact customer inputs before attaching them to public issues. Keep the original internally until the minimized reproducer is confirmed.

## Incident response: start to resolution

1. **Declare severity.** Mark whether this is install failure, parser crash, timeout, wrong-code/miscompilation, performance regression, or release-artifact problem.
2. **Freeze evidence.** Save the original `.ll`/LRIR input, command transcript, binary/object output, logs, host metadata, and artifact checksums.
3. **Reproduce on current `main`.** If it reproduces, continue triage. If not, bisect between the reported version and `main`.
4. **Minimize.** Use `scripts/reduce_ci_failure.sh` for `.ll` failures and store an evidence package.
5. **Bucket and label.** Follow `docs/crash_triage_runbook.md`; assign `bug` plus `crash`, `timeout`, `miscompilation`, or `performance` as applicable.
6. **Assess release impact.** If a stable/RC artifact is affected, apply `docs/release_candidate_protocol.md` rollback triggers and communicate status in the checklist/release issue.
7. **Fix through PR.** Link the issue, include reproducer tests, and wait for CI to go green.
8. **Verify resolution.** Re-run the original and minimized reproducers, relevant conformance/platform/perf gates, and release artifact verification if artifacts changed.
9. **Close the loop.** Comment with fixed commit, validation commands, remaining known issues, and any docs/runbook updates.

## Contributor triage paths

### Parser or verifier bug

- Reproduce with `llvm-ir-parser` tests or the `llvm-ir-min` CLI.
- Minimize to a single `.ll` file.
- Add a parser regression test and, when interop is involved, compare against `llvm-as` from a supported LLVM version.
- Link `docs/interoperability_conformance.md` if the failure crosses LLVM tool boundaries.

### Codegen crash or wrong-code

- Preserve the emitted object file and disassembly when available.
- Run the golden codegen gate and relevant backend tests.
- For debug/unwind failures, include `readelf`, `llvm-dwarfdump`, or `llvm-readobj` output when available.
- Add or update a locked golden test only with maintainer approval; see `docs/golden_codegen_gate.md`.

### Performance regression

- Run `cargo +stable bench -p llvm-in-rust-bench --bench pipeline -- --save-baseline <name>`.
- Compare against `perf/budgets.json` using `scripts/perf_budget.py`.
- Label intentional regressions with `perf-regression-accepted` only after recording the rationale.
- Include hardware, OS, Rust version, and warm/cold cache notes.

### Release artifact problem

- Run `scripts/release_artifacts.sh verify --out-dir <bundle>`.
- Compare `SHA256SUMS`, detached signatures, `release-metadata.json`, and `RELEASE_PROVENANCE.md`.
- Treat checksum/signature mismatch as a rollback trigger until proven otherwise.
- Follow `docs/release_artifact_pipeline.md` and `docs/release_candidate_protocol.md`.

## FAQ: common integration failures

**`cargo build --locked` fails after dependency changes**

Regenerate and commit `Cargo.lock` intentionally. Do not bypass `--locked` for release or production validation.

**LLVM 14 or older `.ll` files fail to parse**

Typed pointers are unsupported. Upgrade inputs with LLVM 15 opaque-pointer mode before passing them to LLVM-in-Rust.

**An object file differs between two builds**

Confirm the same Git commit, `Cargo.lock`, stable Rust version, target triple, and `SOURCE_DATE_EPOCH`. Then compare `SHA256SUMS` and provenance metadata.

**CI fails only on Windows/macOS**

Check `docs/platform_support_policy.md` and `docs/platform_known_issues.json`. Reproduce with `scripts/platform_matrix.sh host-core` on the affected host if possible.

**A benchmark exceeds the budget by a small amount**

Rerun once to rule out noise. If still over budget, attach the Criterion report and either optimize before merge or document an accepted regression.

**A fuzz or stress run produces a huge input**

Minimize with `scripts/reduce_ci_failure.sh`, attach the evidence package, and store the original artifact link for maintainers.

## Runbook index

- Release artifacts: `docs/release_artifact_pipeline.md`
- RC and rollback: `docs/release_candidate_protocol.md`
- Crash and miscompilation triage: `docs/crash_triage_runbook.md`
- Interoperability conformance: `docs/interoperability_conformance.md`
- Platform support: `docs/platform_support_policy.md`
- Golden codegen: `docs/golden_codegen_gate.md`
- Performance budget: `docs/performance_budget_gate.md`
- Sanitizer/UB gates: `docs/sanitizer_ub_gate.md`
