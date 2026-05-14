#!/usr/bin/env python3
"""Output code-quality gate for issue #208.

The gate compiles the canonical kernels in bench/codegen/ with this backend and
with LLVM llc -O2, counts emitted instructions from objdump output, and compares
current metrics against bench/codegen/baselines.json. Locked baselines may be
updated only with --update-baselines --bless and a sign-off comment.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[1]
BENCH_DIR = ROOT / "bench" / "codegen"
BASELINE_PATH = BENCH_DIR / "baselines.json"
TARGET_DIR = ROOT / "target" / "codegen-quality"
REPORT_PATH = ROOT / "codegen-quality-report.md"
THRESHOLD = 0.10

INSTR_RE = re.compile(r"^\s*[0-9a-fA-F]+:\s+(?:[0-9a-fA-F]{2}\s+)+\S+")


def run(cmd: list[str], *, cwd: Path = ROOT, stdout: int | None = None) -> subprocess.CompletedProcess[str]:
    return subprocess.run(cmd, cwd=cwd, text=True, stdout=stdout, stderr=subprocess.PIPE, check=True)


def require_tool(names: list[str]) -> str:
    for name in names:
        found = shutil.which(name)
        if found:
            return found
    raise SystemExit(f"required tool not found on PATH: one of {', '.join(names)}")


def count_objdump_instructions(objdump: str, obj: Path) -> int:
    out = run([objdump, "-d", str(obj)], stdout=subprocess.PIPE).stdout or ""
    return sum(1 for line in out.splitlines() if INSTR_RE.match(line))


def objdump_section_size(objdump: str, obj: Path, section: str) -> int:
    out = run([objdump, "-h", str(obj)], stdout=subprocess.PIPE).stdout or ""
    for line in out.splitlines():
        parts = line.split()
        if len(parts) >= 3 and parts[1] == section:
            return int(parts[2], 16)
    raise RuntimeError(f"section {section} not found in {obj}")


def cargo_stable_cmd() -> list[str]:
    # GitHub's rustup-managed cargo accepts `cargo +stable ...`; Homebrew cargo
    # may not, so prefer rustup when it is available.
    if shutil.which("rustup"):
        return ["rustup", "run", "stable", "cargo"]
    return ["cargo", "+stable"]


def compile_ours(objdump: str) -> dict[str, Any]:
    emit_dir = TARGET_DIR / "ours"
    proc = run(
        cargo_stable_cmd()
        + [
            "run",
            "-q",
            "-p",
            "llvm-in-rust",
            "--bin",
            "codegen-quality",
            "--",
            str(BENCH_DIR),
            str(emit_dir),
        ],
        stdout=subprocess.PIPE,
    )
    data = json.loads(proc.stdout or "{}")
    for metrics in data["kernels"].values():
        obj = Path(metrics["object_path"])
        metrics["instruction_count"] = count_objdump_instructions(objdump, obj)
        metrics.pop("object_path", None)
    return data["kernels"]


def compile_llc(llc: str, objdump: str) -> dict[str, Any]:
    out_dir = TARGET_DIR / "llc-o2"
    out_dir.mkdir(parents=True, exist_ok=True)
    metrics: dict[str, Any] = {}
    for ll in sorted(BENCH_DIR.glob("*.ll")):
        obj = out_dir / f"{ll.stem}.o"
        run([llc, "-O2", "-filetype=obj", str(ll), "-o", str(obj)])
        metrics[ll.stem] = {
            "instruction_count": count_objdump_instructions(objdump, obj),
            "text_bytes": objdump_section_size(objdump, obj, ".text"),
        }
    return metrics


def load_baseline() -> dict[str, Any]:
    try:
        return json.loads(BASELINE_PATH.read_text())
    except FileNotFoundError:
        raise SystemExit(f"missing baseline {BASELINE_PATH}; run make update-baselines with sign-off")


def compare_metric(errors: list[str], kernel: str, producer: str, metric: str, actual: int, expected: int) -> None:
    allowed = max(1, int(expected * (1.0 + THRESHOLD) + 0.9999))
    if actual > allowed:
        errors.append(
            f"{kernel} {producer}.{metric}: actual {actual} exceeds baseline {expected} by >10% (limit {allowed})"
        )


def compare(actual: dict[str, Any], baseline: dict[str, Any]) -> list[str]:
    errors: list[str] = []
    for kernel, expected in baseline["kernels"].items():
        if kernel not in actual["kernels"]:
            errors.append(f"missing kernel metrics: {kernel}")
            continue
        got = actual["kernels"][kernel]
        for producer in ("ours", "llc_o2"):
            if producer not in got:
                errors.append(f"missing {producer} metrics for {kernel}")
                continue
            for metric in ("instruction_count", "text_bytes"):
                compare_metric(errors, kernel, producer, metric, int(got[producer][metric]), int(expected[producer][metric]))
        compare_metric(
            errors,
            kernel,
            "ours",
            "spill_reload_instructions",
            int(got["ours"].get("spill_reload_instructions", 0)),
            int(expected["ours"].get("spill_reload_instructions", 0)),
        )
        compare_metric(
            errors,
            kernel,
            "ours",
            "prologue_epilogue_bytes",
            int(got["ours"].get("prologue_epilogue_bytes", 0)),
            int(expected["ours"].get("prologue_epilogue_bytes", 0)),
        )
    return errors


def make_report(actual: dict[str, Any], errors: list[str]) -> str:
    lines = [
        "# Codegen quality gate",
        "",
        f"Threshold: fail when a locked metric regresses by more than {int(THRESHOLD * 100)}%.",
        "",
        "| Kernel | Ours instr | Ours spills | Ours text bytes | Pro/epi bytes | llc -O2 instr | llc -O2 text bytes |",
        "| --- | ---: | ---: | ---: | ---: | ---: | ---: |",
    ]
    for kernel, metrics in actual["kernels"].items():
        ours = metrics["ours"]
        llc = metrics["llc_o2"]
        lines.append(
            "| {kernel} | {oi} | {sp} | {ot} | {pe} | {li} | {lt} |".format(
                kernel=kernel,
                oi=ours["instruction_count"],
                sp=ours["spill_reload_instructions"],
                ot=ours["text_bytes"],
                pe=ours["prologue_epilogue_bytes"],
                li=llc["instruction_count"],
                lt=llc["text_bytes"],
            )
        )
    lines.append("")
    if errors:
        lines.append("## Regressions")
        lines.extend(f"- {e}" for e in errors)
    else:
        lines.append("No regressions beyond the locked threshold.")
    lines.append("")
    return "\n".join(lines)


def build_actual(llc: str, objdump: str) -> dict[str, Any]:
    ours = compile_ours(objdump)
    llc_o2 = compile_llc(llc, objdump)
    kernels: dict[str, Any] = {}
    for name in sorted(ours):
        kernels[name] = {"ours": ours[name], "llc_o2": llc_o2[name]}
    return {
        "version": 1,
        "threshold": THRESHOLD,
        "kernels": kernels,
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--update-baselines", action="store_true")
    parser.add_argument("--bless", action="store_true")
    parser.add_argument("--report", default=str(REPORT_PATH))
    args = parser.parse_args()

    llc = require_tool([
        "llc",
        "llc-19",
        "/usr/lib/llvm-19/bin/llc",
        "/opt/homebrew/opt/llvm/bin/llc",
        "/opt/homebrew/opt/llvm@19/bin/llc",
    ])
    objdump = require_tool([
        "llvm-objdump",
        "llvm-objdump-19",
        "/usr/lib/llvm-19/bin/llvm-objdump",
        "/opt/homebrew/opt/llvm/bin/llvm-objdump",
        "/opt/homebrew/opt/llvm@19/bin/llvm-objdump",
    ])

    actual = build_actual(llc, objdump)

    if args.update_baselines:
        signoff = os.environ.get("CODEGEN_QUALITY_BASELINE_SIGNOFF", "").strip()
        if not args.bless or not signoff:
            raise SystemExit(
                "refusing to update baselines without --bless and CODEGEN_QUALITY_BASELINE_SIGNOFF"
            )
        actual["baseline_signoff"] = signoff
        BASELINE_PATH.write_text(json.dumps(actual, indent=2, sort_keys=True) + "\n")
        print(f"updated {BASELINE_PATH}")
        return 0

    baseline = load_baseline()
    errors = compare(actual, baseline)
    report = make_report(actual, errors)
    report_path = Path(args.report)
    report_path.write_text(report)
    if os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(os.environ["GITHUB_STEP_SUMMARY"], "a", encoding="utf-8") as fh:
            fh.write(report)
            fh.write("\n")
    print(report)
    if errors:
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
