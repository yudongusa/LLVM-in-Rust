Fix a specific GitHub issue with a dedicated branch, commit, PR, and squash-merge.

## Usage
```
/fix-issue <issue-number>
```

## What this skill does
Reads the GitHub issue, traces the root cause in the codebase, implements the
minimal correct fix, adds a regression test, and merges a PR — fully autonomously.

## Execution steps

### 1. Read the issue
```bash
gh issue view <N>
```
Understand: what is wrong, which files are involved, and what the correct
behaviour should be.

### 2. Read the relevant source files
Use Read/Grep/Glob to locate and read every file mentioned in the issue before
writing a single line of code. Never edit code you haven't read.

### 3. Branch from latest main
```bash
git fetch origin
git checkout -b fix/issue-<N>-<slug> origin/main
```
`<slug>` is a 2–4 word kebab-case summary of the fix (e.g. `udiv-unsigned-div`).
Always branch from `origin/main`, not a local tracking branch.

### 4. Implement the fix
- Make the **minimum change** that correctly addresses the root cause.
- Do not refactor unrelated code, add comments to unchanged functions, or
  introduce new abstractions unless they are strictly required by the fix.
- If a new opcode or helper is needed, add it in the natural place following
  the existing file's style and conventions.

### 5. Add a regression test
Add at least one test in the existing `#[cfg(test)] mod tests` block that:
- Would have **failed** with the old code.
- **Passes** with the fix.
- Is named descriptively (e.g. `udiv_uses_div_r_not_idiv_r`).

If relevant, also add a regression guard test that verifies the complementary
case still works (e.g. `sdiv_uses_idiv_r` alongside the unsigned fix).

### 6. Verify
```bash
cargo test -p <affected-crate>   # fast feedback on the changed crate
cargo test                        # full workspace — must be all green
```
Never commit if any test fails.

### 7. Commit
```bash
git add <specific files only — never "git add -A">
git commit -m "$(cat <<'EOF'
Fix <short description of what was wrong and what the fix does> (closes #<N>)

<1–3 sentence explanation of root cause and approach.>

Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>
EOF
)"
```

### 8. Push, open PR, and merge
```bash
git push -u origin fix/issue-<N>-<slug>

gh pr create \
  --title "Fix <description> (issue #<N>)" \
  --body "$(cat <<'EOF'
## Summary
<bullet points of what changed>

## Root cause
<one paragraph>

## Test plan
- [ ] <new test name> — <what it verifies>
- [ ] All <X> existing tests continue to pass

Closes #<N>

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"

gh pr merge <PR-N> --squash
```
Do NOT pass `--delete-branch` — `main` may be checked out in another worktree.

## Pitfalls to avoid
- **Branch from `origin/main`**, not `main` — the latter may be another worktree.
- **Stage specific files** — `git add -A` can accidentally include `target/` or
  `.env` files.
- **`--no-verify` is forbidden** — if a pre-commit hook fails, fix the underlying
  issue and create a fresh commit.
- **Scope creep** — if you discover a second bug while fixing the first, open a new
  issue and handle it in a separate PR.
