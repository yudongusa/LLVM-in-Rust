# Issue #133 Plan

## Acceptance Targets

- Add a tracked Windows debug architecture document in-repo.
- Implement first usable milestone: COFF emission with a CodeView debug section for source/line context.
- Add tests that fail if the COFF/CodeView emission regresses.
- Provide CI-compatible checks or explicit external validation steps for Windows toolchains.

## Suggested Order

1. Add COFF serializer + `ObjectFormat::Coff` plumbing.
2. Emit `.debug$S` when debug metadata is available.
3. Add unit/integration tests for structure and payload.
4. Document Windows validation commands and current scope limits.
5. Run full test suite and post review findings.
