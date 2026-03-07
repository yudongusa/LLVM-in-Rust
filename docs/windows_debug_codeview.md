# Windows Debug Pipeline (Issue #133)

This document defines the staged Windows debug-info architecture for LLVM-in-Rust.

## Goal

Carry source debug metadata from `.ll` (`!dbg`, `!DILocation`) into Windows-consumable object artifacts and then into PDB-compatible flows.

## Pipeline

1. IR parser preserves metadata attachments and `DILocation` nodes.
2. Target lowering threads debug locations into machine instructions.
3. Object emitter writes COFF object files (`ObjectFormat::Coff`).
4. When debug rows are present, emitter adds CodeView section `.debug$S`.
5. Windows link stage (`lld-link` / `link.exe`) can consume COFF + CodeView and produce PDB in later milestones.

## Milestone M1 (implemented)

- `ObjectFormat::Coff` support in object serialization.
- `.debug$S` section emission with a minimal CodeView payload:
  - `CV_SIGNATURE_C13` header
  - one `DEBUG_S_SYMBOLS` subsection containing source identity and line span hints
- Regression tests verify:
  - COFF machine/header correctness
  - `.debug$S` section presence and payload signature when debug metadata exists

## Validation

### Local tests

```bash
cargo +stable test -p llvm-codegen
cargo +stable test -q
```

### External inspection (cross-platform with LLVM tools)

```bash
# Inspect sections in emitted COFF object
llvm-readobj --sections /tmp/eval_predicate.obj
```

### Windows native check (documented step)

```powershell
# produce PDB in a future milestone once linker integration is wired
lld-link /DEBUG /ENTRY:main /SUBSYSTEM:CONSOLE /OUT:prog.exe eval_predicate.obj
```

## Current limitations

- `.debug$S` payload is intentionally minimal and not yet a full CodeView symbol/line program.
- No in-repo PDB writer yet; PDB generation is delegated to external linkers.
- Full debugger-stepping validation (WinDbg/VS) remains a follow-up milestone.

## Follow-up milestones

- M2: richer CodeView symbol records for functions/locals/line blocks.
- M3: Windows CI job that links COFF object and asserts PDB/debugger-visible line info.
- M4: target coverage expansion beyond x86_64.
