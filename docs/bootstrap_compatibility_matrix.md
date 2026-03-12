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

## Blocker Matrix (living)

| Area | Current status | Blocking gaps | Next action |
|---|---|---|---|
| Stage A core subset | ✅ gated in CI | none identified | keep green as regression gate |
| Stage B expanded suite | ✅ gated in CI | no dedicated alloc IR fixture family yet | add alloc-oriented fixture pack |
| Stage C libc compatibility | 🚧 not started | libc fixture corpus + ABI integration harness missing | define Stage C fixture spec + initial tests |
| Stage D bootstrap experiments | 🚧 not started | frontend/bootstrap harness missing | draft experiment plan + minimal milestone |

## Notes

This is intentionally incremental. Stage A/B are now hard CI gates, while Stage C/D remain tracked roadmap work.
