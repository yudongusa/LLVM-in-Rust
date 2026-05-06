# Performance budget gate

Issue #179 defines the M1 performance gate: PRs should not silently regress compile-time or runtime behavior.

## Suites and thresholds

Budgets live in `perf/budgets.json`.

- `compile_time`: Criterion benchmarks from `llvm-bench`; a PR fails if any tracked benchmark regresses by more than **5%** against the base branch on the same CI runner.
- `runtime`: currently covered by the differential, smoke-oracle, CSmith, and fuzz gates; the budget file reserves a **3%** threshold for future runtime microbenchmarks once a stable suite exists.

## CI behavior

`.github/workflows/performance-budget.yml` runs on pull requests to `main`:

1. Check out the base commit and run `cargo +stable bench -p llvm-bench`.
2. Check out the PR head and run the same benchmarks.
3. Compare Criterion mean estimates with `scripts/perf_budget.py`.
4. Upload `perf-budget-report.md` as a CI artifact.

The comparison is base-vs-head on the same runner to avoid false failures from machine-to-machine variance.

## Accepted regression override

Use the `perf-regression-accepted` label only when maintainers explicitly accept a regression. The PR description or review must include:

- the benchmark names and observed deltas,
- why the regression is acceptable,
- the issue or follow-up PR that tracks recovery if the regression is temporary.

The workflow still uploads a report when the override is used, but the budget step does not block the PR.

## Trend history

Each PR uploads its budget report artifact. Release managers should copy representative green reports into release notes or a future persistent dashboard once enough history exists for trend analysis.
