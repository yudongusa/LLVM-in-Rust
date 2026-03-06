# AGENTS.md — Agentic Development Guide

This file documents the agentic workflow used to develop LLVM-in-Rust with
Claude Code.  It exists so that Claude can operate **autonomously** on this
project with minimal back-and-forth, following the same patterns used
throughout Phases 1–4.

---

## Development Lifecycle

Every feature follows this five-stage cycle, executed end-to-end without
user prompts at each step:

```
Plan → Implement → PR Review → Fix Review Findings → Merge
```

| Stage | Slash skill | Description |
|-------|-------------|-------------|
| Implement a phase | `/implement-phase` | Branch → code → test → commit → PR |
| Review implementation PR | `/review-and-fix` | Review diff/tests → post PR feedback → fix findings |
| Fix one issue | `/fix-issue <N>` | Read issue → fix → test → PR → review feedback → merge |

### Mandatory PR Review Feedback Step (for implementation PRs)

Before merging an implementation PR, the agent must:

1. Review the PR diff and changed tests with a code-review mindset (correctness, regressions, missing tests).
2. Post review feedback to the PR (`gh pr review --comment` or `gh pr comment`) with concrete findings.
3. If any issue is found, push a fix commit, then post a follow-up comment summarizing what was fixed.
4. Merge only when checks are green and no unresolved review findings remain.

---

## Git Workflow Conventions

These rules prevent common mistakes in the multi-worktree setup:

| Rule | Reason |
|------|--------|
| Always branch from `origin/main`, not `main` | `main` is checked out in the primary worktree; checking it out again fails |
| `gh pr merge <N> --squash` (no `--delete-branch`) | Same worktree conflict |
| Stage specific files, never `git add -A` | Avoids accidentally committing `target/` or secret files |
| Never use `--no-verify` | Fix the hook failure instead |
| Run `cargo test` before every commit | All tests must be green |
| Post at least one PR review feedback comment before merge | Captures reviewer reasoning and findings in GitHub history |

**Branch naming:**
- Features: `feature/phase<N>-<slug>` (e.g. `feature/phase4-x86-backend`)
- Fixes: `fix/issue-<N>-<slug>` (e.g. `fix/issue-30-mov-to-preg`)

---

## Agent Usage Guide

### rust-stable-compat agent
Use for issue #90 and any nightly-to-stable migration work.

```
Invoke: $rust-stable-compat
When:   Removing `#![feature(...)]`, migrating benches to stable,
        updating CI/docs, and validating stable build/test commands.
Skill:  skills/rust-stable-compat/SKILL.md
```

### simd-vectorization agent
Use for issue #86 and any x86 SIMD vector-lowering work.

```
Invoke: $simd-vectorization
When:   Adding vector IR lowering, SSE4.2/AVX2 emission, and target-feature
        gating in the x86 backend.
Skill:  skills/simd-vectorization/SKILL.md
```

### ipa-optimizer agent
Use for issue #87 and inter-procedural optimization work.

```
Invoke: $ipa-optimizer
When:   Building call-graph analysis, IPCP/dead-argument module passes, and
        integrating IPA into O3.
Skill:  skills/ipa-optimizer/SKILL.md
```

### Plan agent
Use **before** starting a new phase or a non-trivial fix.

```
Invoke: Agent tool with subagent_type="Plan"
When:   Implementing a new crate, designing a data structure, or planning
        multiple-file changes across >3 files.
Output: Step-by-step plan written to /Users/yudong/.claude/plans/<name>.md
        followed by ExitPlanMode.
```

### Explore agent
Use for **codebase searches** when the location of something is unknown.

```
Invoke: Agent tool with subagent_type="Explore"
When:   Looking for where a trait is implemented, all uses of a type,
        or understanding how an existing subsystem works.
Levels: "quick" (single grep), "medium" (several files), "very thorough" (deep)
```

### general-purpose agent (background)
Use for **parallel independent work** — e.g. running tests in the background
while writing another file, or fetching GitHub issue data while reading source.

```
Invoke: Agent tool with subagent_type="general-purpose", run_in_background=true
When:   The sub-task is independent of the current work and would block
        the main thread if run synchronously.
```

### When NOT to use agents
- Reading a specific known file → use `Read` directly
- Searching for a specific class or function → use `Grep` directly
- Simple one-file edits → use `Edit` directly

---

## Code Quality Standards

Every PR merged into `main` must satisfy:

1. **`cargo test` all green** — no skipped tests, no `#[ignore]` added.
2. **Targeted tests** — every bug fix adds at least one regression test named
   after what it verifies (e.g. `udiv_uses_div_r_not_idiv_r`).
3. **Minimal diff** — only the lines necessary to fix the bug or implement
   the feature; no reformatting or unrelated cleanup.
4. **Squash merge** — one commit per PR on `main`; branch history preserved
   in the PR.
5. **Closes #N in commit message** — so GitHub auto-closes the issue.

---

## Phase Roadmap

| Phase | Crates | Status |
|-------|--------|--------|
| 1 — IR Foundation | `llvm-ir`, `llvm-ir-parser` | ✅ Complete |
| 2 — Analysis | `llvm-analysis` | ✅ Complete |
| 3 — Optimizations | `llvm-transforms` | ✅ Complete |
| 4 — x86_64 Backend | `llvm-codegen`, `llvm-target-x86` | ✅ Complete + reviewed |
| 5 — AArch64 + Bitcode | `llvm-target-arm`, `llvm-bitcode` | 🔲 Next |

For Phase 5 details see the open issue #7 and `CLAUDE.md` §"Phase 5".

---

## Memory & Context

Persistent cross-session notes live at:
```
/Users/yudong/.claude/projects/-Users-yudong-work-claude-LLVM-in-Rust/memory/MEMORY.md
```

**Always read `MEMORY.md` at the start of a session** to avoid re-doing work.
**Always update `MEMORY.md` after a phase completes** with new status, key
file paths, and design decisions.

Topic files in the same directory (`debugging.md`, `patterns.md`, etc.) hold
deeper notes; link to them from `MEMORY.md`.

---

## Commit Message Format

```
<imperative subject line, ≤72 chars> (closes #N)

<optional body: root cause, approach, notable decisions>
<blank line if body present>
Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>
```

PR body template:
```markdown
## Summary
- <bullet>

## Root cause / Design rationale
<paragraph>

## Test plan
- [ ] <new test name> — <what it verifies>
- [ ] All <X> existing tests pass

Closes #N

🤖 Generated with [Claude Code](https://claude.com/claude-code)
```
