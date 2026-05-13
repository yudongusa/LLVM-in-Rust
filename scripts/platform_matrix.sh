#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/platform_matrix.sh <lane>

Lanes:
  host-core       Run Tier-1 host smoke checks for the current OS.
  target-x86_64   Validate x86-64 artifact-generation crates.
  target-aarch64  Validate AArch64 artifact-generation crates.
  target-rv64gc   Validate RV64GC artifact-generation crates.
  known-issues   Validate docs/platform_known_issues.json shape.
USAGE
}

lane="${1:-}"
if [[ -z "$lane" || "$lane" == "-h" || "$lane" == "--help" ]]; then
  usage
  exit 0
fi

run_host_core() {
  echo "[platform] Tier-1 host core checks on $(uname -s)"
  cargo +stable test -p llvm-in-rust-ir --lib
  cargo +stable test -p llvm-in-rust-ir-parser --test smoke -- --nocapture
  cargo +stable test -p llvm-in-rust-codegen --test linker_compat tool_presence_report_is_accessible -- --nocapture
}

run_target_x86_64() {
  echo "[platform] x86-64 artifact-generation crate checks"
  cargo +stable check -p llvm-in-rust-target-x86 --target x86_64-unknown-linux-gnu
}

run_target_aarch64() {
  echo "[platform] AArch64 artifact-generation crate checks"
  cargo +stable check -p llvm-in-rust-target-arm --target aarch64-unknown-linux-gnu
}

run_target_rv64gc() {
  echo "[platform] RV64GC artifact-generation crate checks"
  cargo +stable check -p llvm-in-rust-target-riscv --target riscv64gc-unknown-linux-gnu
}

validate_known_issues() {
  python3 - <<'PY'
import json
from pathlib import Path

path = Path("docs/platform_known_issues.json")
data = json.loads(path.read_text())
assert data.get("schema_version") == 1, "schema_version must be 1"
assert data.get("policy") == "docs/platform_support_policy.md", "policy path mismatch"
issues = data.get("issues")
assert isinstance(issues, list), "issues must be a list"
required = {"id", "tier", "category", "status", "owner", "eta", "summary"}
valid_tiers = {"tier-1", "tier-2"}
valid_categories = {"host", "target", "abi", "toolchain"}
valid_status = {"open", "accepted", "mitigated", "closed"}
for idx, issue in enumerate(issues):
    missing = sorted(required - set(issue))
    assert not missing, f"issue[{idx}] missing keys: {missing}"
    assert issue["tier"] in valid_tiers, f"issue[{idx}] invalid tier"
    assert issue["category"] in valid_categories, f"issue[{idx}] invalid category"
    assert issue["status"] in valid_status, f"issue[{idx}] invalid status"
    for key in ["owner", "eta", "summary"]:
        assert str(issue[key]).strip(), f"issue[{idx}] {key} must be non-empty"
print(f"validated {path} with {len(issues)} tracked issue(s)")
PY
}

case "$lane" in
  host-core) run_host_core ;;
  target-x86_64) run_target_x86_64 ;;
  target-aarch64) run_target_aarch64 ;;
  target-rv64gc) run_target_rv64gc ;;
  known-issues) validate_known_issues ;;
  *)
    usage >&2
    exit 2
    ;;
esac
