#!/usr/bin/env python3
"""Compare Criterion benchmark estimates and fail on budget regressions.

The script intentionally compares base and PR measurements produced on the same
CI runner instead of relying on a checked-in nanosecond baseline. That keeps the
signal stable across GitHub runner hardware while still enforcing per-PR deltas.
"""
from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path


def load_estimates(root: Path) -> dict[str, float]:
    results: dict[str, float] = {}
    for estimate in root.rglob("estimates.json"):
        # Criterion writes estimates under the current sample directory (`new`)
        # and under any explicit `--save-baseline <name>` directory. The CI
        # workflow uses explicit `base`/`head` baselines in separate checkouts.
        if estimate.parent.name not in {"new", "base", "head"}:
            continue
        rel = estimate.parent.parent.relative_to(root)
        name = "/".join(rel.parts)
        # Criterion stores names passed as `group.bench_function("name")` as
        # `group/name`, but a flat `c.bench_function("group/name")` may appear
        # as `group_name` on disk. Normalize the existing pipeline benchmarks
        # so both styles can be budgeted consistently.
        if name.startswith("pipeline_"):
            name = name.replace("_", "/", 1)
        try:
            data = json.loads(estimate.read_text())
            point = data["mean"]["point_estimate"]
        except (OSError, KeyError, TypeError, json.JSONDecodeError) as exc:
            raise SystemExit(f"failed to read Criterion estimate {estimate}: {exc}") from exc
        results[name] = float(point)
    return results


def pct_delta(base: float, head: float) -> float:
    if base <= 0:
        return 0.0 if head <= 0 else float("inf")
    return ((head - base) / base) * 100.0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base", required=True, type=Path, help="base target/criterion directory")
    parser.add_argument("--head", required=True, type=Path, help="head target/criterion directory")
    parser.add_argument("--budgets", required=True, type=Path, help="perf/budgets.json")
    parser.add_argument("--report", required=True, type=Path, help="markdown report path")
    parser.add_argument("--allow-regression", action="store_true", help="report but do not fail budget violations")
    args = parser.parse_args()

    budgets = json.loads(args.budgets.read_text())
    default_threshold = float(budgets.get("default_threshold_percent", 5.0))
    base = load_estimates(args.base)
    head = load_estimates(args.head)

    expected: dict[str, float] = {}
    for suite in budgets.get("suites", {}).values():
        threshold = float(suite.get("threshold_percent", default_threshold))
        for bench in suite.get("benchmarks", []):
            expected[bench] = threshold

    missing = sorted(name for name in expected if name not in base or name not in head)
    rows: list[tuple[str, float, float, float, float, str]] = []
    failures: list[str] = []

    for name in sorted(expected):
        if name in missing:
            continue
        delta = pct_delta(base[name], head[name])
        threshold = expected[name]
        status = "FAIL" if delta > threshold else "PASS"
        if status == "FAIL":
            failures.append(f"{name}: +{delta:.2f}% > +{threshold:.2f}%")
        rows.append((name, base[name], head[name], delta, threshold, status))

    args.report.parent.mkdir(parents=True, exist_ok=True)
    with args.report.open("w", encoding="utf-8") as fh:
        fh.write("# Performance budget report\n\n")
        fh.write(f"Override label: `{budgets.get('override_label', 'perf-regression-accepted')}`\n\n")
        fh.write("| Benchmark | Base mean (ns) | Head mean (ns) | Delta | Budget | Status |\n")
        fh.write("|---|---:|---:|---:|---:|---|\n")
        for name, base_ns, head_ns, delta, threshold, status in rows:
            fh.write(f"| `{name}` | {base_ns:.0f} | {head_ns:.0f} | {delta:+.2f}% | +{threshold:.2f}% | {status} |\n")
        if missing:
            fh.write("\n## Missing expected benchmarks\n\n")
            for name in missing:
                fh.write(f"- `{name}`\n")
        if failures:
            fh.write("\n## Regressions above budget\n\n")
            for failure in failures:
                fh.write(f"- {failure}\n")

    if missing:
        print("missing expected benchmarks:", ", ".join(missing), file=sys.stderr)
        return 1
    if failures and not args.allow_regression:
        print("performance budget exceeded:", file=sys.stderr)
        for failure in failures:
            print(f"  {failure}", file=sys.stderr)
        return 1
    print(f"performance budget report written to {args.report}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
