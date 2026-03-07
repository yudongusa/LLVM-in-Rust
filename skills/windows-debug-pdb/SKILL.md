---
name: windows-debug-pdb
description: Implement issue #133 by adding Windows COFF + CodeView debug emission milestones, validation tests, and documentation for the staged PDB pipeline.
---

# Windows Debug / PDB

Use this skill to execute issue #133 with incremental, test-backed Windows debug support.

## Workflow

1. Add/maintain in-repo architecture doc for metadata -> CodeView -> PDB path.
2. Implement first usable milestone in codegen/emitter (COFF + `.debug$S` CodeView payload).
3. Add tests that validate emitted COFF structure and debug section presence.
4. Add external validation steps (`llvm-readobj`, `lld-link`/`link.exe`, debugger checks) with graceful fallback if tools are unavailable.
5. Update README with explicit Windows commands and current limitations.
6. Review PR, run full tests, open issue(s) for any findings, fix in same PR, and post review summary.

## Minimum validation

```bash
cargo +stable test -p llvm-codegen
cargo +stable test -q
```

## Notes

- Keep milestones explicit; avoid claiming full PDB support before symbol/line records are debugger-verified.
- Prefer deterministic assertions on section names, signatures, and source/line payload.
