---
name: linker-compat
description: Implement issue #91 by validating ELF/Mach-O objects against real linkers/tools, adding linker compatibility integration tests in llvm-codegen, fixing format gaps, and documenting exact link commands.
---

# Linker Compatibility

Use this skill to execute issue #91 end-to-end with external-tool validation.

## Workflow

1. Add integration tests in `llvm-codegen` that emit objects and validate linker/tool compatibility.
2. Run and triage `cc`/`ld`/`lld`/`readelf`/`nm` (and macOS tools when applicable).
3. Fix object-format conformance issues found by the tests.
4. Add regression coverage for each fix.
5. Update README with exact link commands.
6. Review PR, run full tests, open issue(s) for findings, fix in same PR, and post review summary.

## Step 1: Test Harness

- Add `src/llvm-codegen/tests/linker_compat.rs`.
- Include at least:
  - ELF/Linux linker compatibility path (`cc` link + run)
  - Mach-O/macOS path (`cc`/`ld -r`) under platform guard
- If external tool is unavailable, skip test gracefully unless strict mode is set.

## Step 2: Tool Validation

- Validate object structure with available tools (`readelf`, `nm`, `objdump`, `otool`).
- Prefer deterministic assertions: exit code, symbol presence, relocation section presence.

## Step 3: Fix Format Gaps

- Address concrete linker/debugger compatibility failures surfaced by tests.
- Add focused regression tests for each root cause.

## Step 4: README Update

- Document exact link invocations for produced `.o` files on Linux and macOS.
- Keep commands copy-paste ready.

## Step 5: Validation

Run at minimum:

```bash
cargo +stable test -p llvm-codegen
cargo +stable test
```

If required external tools are unavailable in environment, document exactly which tools are missing.

## Step 6: Review/Test/Issue+Fix Loop

- Review PR diff and run targeted + full test suites.
- If bugs are found, open issue(s), fix in same PR branch, and push follow-up commits.
- Post PR review summary comment with findings/fixes before merge.

## Resources

- Use [`references/issue-91-plan.md`](references/issue-91-plan.md) for acceptance checklist.
- Use [`scripts/linker_toolcheck.sh`](scripts/linker_toolcheck.sh) to quickly detect local tool availability.
