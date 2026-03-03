Review the most recently merged PR for correctness issues, open a GitHub issue for
each problem found, then fix every issue in its own dedicated PR.

## Usage
```
/review-and-fix
/review-and-fix <PR-number>
```
If a PR number is supplied, review that PR; otherwise review the most recently merged PR.

## What this skill does
Runs a thorough code review, catalogs all bugs as GitHub issues, then autonomously
fixes them one by one — each fix on its own branch with its own PR — without
requiring the user to prompt for each individual fix.

## Execution steps

### Phase A — Code review

1. **Fetch the PR diff**
   ```bash
   gh pr view <N> --json files,additions,deletions
   gh pr diff <N>
   ```

2. **Deep analysis** — Use the Plan agent to review every changed file for:
   - **Critical bugs**: wrong output (incorrect encodings, wrong opcodes, sign errors)
   - **Logic errors**: conditions always true/false, operands in wrong order
   - **Missing cases**: enum variants not handled, edge cases silently ignored
   - **ABI/calling-convention violations**: wrong registers, missing clobbers
   - **Performance regressions**: unnecessary O(n log n) ops inside loops
   - **Minor issues**: null vs sentinel byte, off-by-one in struct layouts

3. **Categorise** each finding as Critical / Moderate / Minor.

### Phase B — Open GitHub issues

For each distinct problem, open **one issue** with:
```bash
gh issue create \
  --title "<short imperative title>" \
  --body "## Root cause\n…\n## Affected file(s)\n…\n## Expected behaviour\n…\n## Actual behaviour\n…"
```
Use a clear, searchable title (e.g. "emit_mov_to_preg always emits NOP").
Record every issue number for Phase C.

### Phase C — Fix each issue

Repeat the following for every issue, starting with Critical, then Moderate, then Minor:

1. **Branch from latest main**
   ```bash
   git fetch origin
   git checkout -b fix/issue-<N>-<slug> origin/main
   ```

2. **Understand the code** — read the relevant source files before editing.

3. **Implement the minimal fix** — change only what is necessary to correct the bug.
   Do not refactor surrounding code or add unrelated improvements.

4. **Add targeted tests** that would have caught the bug. Place them in the existing
   `#[cfg(test)] mod tests` block of the affected file.

5. **Verify**
   ```bash
   cargo test -p <affected-crate>
   cargo test   # full suite — must be all green
   ```

6. **Commit**
   ```bash
   git add <specific files>
   git commit -m "Fix <short description> (closes #<issue-N>)

   Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>"
   ```

7. **PR and merge**
   ```bash
   git push -u origin fix/issue-<N>-<slug>
   gh pr create --title "Fix <description> (issue #<N>)" \
     --body "## Summary\n…\n## Root cause\n…\n## Test plan\n- [ ] …"
   gh pr merge <PR-N> --squash
   ```
   Do NOT use `--delete-branch`.

8. **Fetch main** before starting the next fix so each branch is current.

## Constraints
- One GitHub issue per distinct problem (do not bundle unrelated bugs).
- One PR per issue (or tightly coupled pair), so git history stays bisectable.
- Never mark an issue as fixed until `cargo test` is fully green.
- If a fix reveals a secondary bug, open a new issue rather than expanding scope.
