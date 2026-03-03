Implement the next incomplete phase of the LLVM-in-Rust compiler project.

## What this skill does
Reads the project plan, implements all files for the next outstanding phase,
runs tests, commits to a feature branch, creates a PR, and merges it — with
no user interaction required beyond invoking this skill.

## Execution steps

### 1. Determine the phase to implement
- Read `CLAUDE.md` — the "Implementation Phases" section lists phases and their completion status.
- Read `/Users/yudong/.claude/projects/-Users-yudong-work-claude-LLVM-in-Rust/memory/MEMORY.md`
  to understand what has already been completed.
- If a plan file exists at `/Users/yudong/.claude/plans/`, load it; otherwise use the
  Plan agent to produce a detailed step-by-step implementation plan before writing any code.

### 2. Create a feature branch
```bash
git fetch origin
git checkout -b feature/phase<N>-<short-description> origin/main
```
Always branch from `origin/main`, never from a local tracking branch, because
`main` may be checked out in another worktree.

### 3. Implement each file
- Read every existing stub file before editing it.
- Follow the architecture and naming conventions already established in the codebase.
- Add `#[cfg(test)] mod tests { … }` blocks at the bottom of each new source file.
- Run `cargo check` after each file to catch type errors early.
- Run `cargo test -p <crate>` once the crate is complete before moving on.

### 4. Full test run
```bash
cargo test
```
All tests must pass before committing. If any test fails, diagnose and fix it —
do not skip or comment out failing tests.

### 5. Commit
Stage only the files changed for this phase (not `target/` or `*.local.*`).
```bash
git add <specific files>
git commit -m "Phase <N>: <description> (closes #<issue>)

Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>"
```

### 6. Push and open PR
```bash
git push -u origin <branch>
gh pr create --title "Phase <N>: <description>" --body "…"
```
The PR body must include: what was implemented, key design decisions, and a
test plan checklist.

### 7. Merge
```bash
gh pr merge <N> --squash
```
Do NOT use `--delete-branch` because `main` may live in another git worktree.

### 8. Update memory
Edit `MEMORY.md` in the auto-memory directory to record the new phase status,
key file paths, and any design decisions made during implementation.

## Constraints
- No new Cargo dependencies unless explicitly called for in `CLAUDE.md`.
- Keep each crate's public API minimal — only expose what downstream crates need.
- Never commit generated files (`target/`, `*.lock` changes from dependency adds).
- If `cargo test` output shows a pre-commit hook failure, fix the underlying issue
  and create a new commit (never use `--no-verify`).
