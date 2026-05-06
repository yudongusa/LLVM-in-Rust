# Bootstrap Compatibility Ladder (Issue #151)

This document defines the staged compatibility path toward bootstrap-grade stability.

## Stage Ladder

- **Stage A — core-level subset fixtures**
  - Goal: prove parser/lowering/codegen stability on foundational integer/control-flow patterns.
  - CI gate: `scripts/compat_stage_a.sh`
- **Stage B — expanded core/alloc proxy suite**
  - Goal: prove stability on broader differential corpus plus debug-metadata continuity.
  - CI gate: `scripts/compat_stage_b.sh`
- **Stage C — libc-oriented fixture/integration set**
  - Goal: validate libc-like ABI/integration expectations across toolchains.
  - Status: planned
- **Stage D — frontend/bootstrap experiments**
  - Goal: self-hosting-adjacent frontend pipeline experiments.
  - Status: long-term planned

## CI Gates (current)

- `compat_stage_a` job (required): Stage A script pass
- `compat_stage_b` job (required): Stage B script pass
- `Sanitizer and UB hardening` workflow: Gate 4 ASan/Miri PR smoke plus nightly/manual/RC TSan lane
- `Platform Matrix Gate` workflow: M2 Tier-1 Linux/macOS/Windows host checks, x86-64/AArch64/RV64GC artifact-generation checks, and known-issues registry validation
- `Interoperability Conformance Gate` workflow: M2 link/debug/ABI/mixed-toolchain conformance lanes with categorized failures

## Blocker Matrix (living)

| Area | Current status | Blocking gaps | Next action |
|---|---|---|---|
| Stage A core subset | ✅ gated in CI | none identified | keep green as regression gate |
| Stage B expanded suite | ✅ gated in CI | no dedicated alloc IR fixture family yet | add alloc-oriented fixture pack |
| Stage C libc compatibility | 🚧 not started | libc fixture corpus + ABI integration harness missing | define Stage C fixture spec + initial tests |
| Stage D bootstrap experiments | 🚧 not started | frontend/bootstrap harness missing | draft experiment plan + minimal milestone |
| Gate 4 sanitizer/UB hardening | ✅ targeted matrix added | full-workspace sanitizer coverage may be too slow/flaky | keep ASan/Miri PR smoke green and expand cautiously |
| M2 platform matrix | ✅ Tier-1 policy + CI added | Tier-2 expansion depends on owned known issues | keep `docs/platform_known_issues.json` current and review before release |
| M2 interoperability conformance | ✅ link/debug/ABI/mixed lanes added | broaden debugger assertions as emitted metadata grows | keep conformance workflow green and categorize failures |

## Notes

This is intentionally incremental. Stage A/B are now hard CI gates, while Stage C/D remain tracked roadmap work.
