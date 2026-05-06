# Platform Support Policy (M2 Gate)

This policy defines the support tiers that must stay visible in CI before a release.

## Support tiers

| Tier | Scope | Required signal | Breakage policy |
|---|---|---|---|
| Tier-1 | Linux, macOS, and Windows host builds for the core workspace | Required on every PR and merge to `main` | Blocks merge/release until fixed or explicitly reclassified |
| Tier-1 targets | `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, `riscv64gc-unknown-linux-gnu` artifact-generation checks | Required on every PR and merge to `main` via the platform matrix workflow | Blocks merge/release unless an accepted known issue exists |
| Tier-2 | Additional host/target combinations not listed above | Best-effort/manual or scheduled coverage | Must be visible in the known-issues registry with owner and ETA |

## Required CI coverage

The `Platform Matrix Gate` workflow provides the M2 merge gate:

- `Tier-1 host matrix`: runs core checks on `ubuntu-24.04`, `macos-14`, and `windows-2022`.
- `Tier-1 artifact generation`: validates target-specific crates for x86-64, AArch64, and RV64GC target triples.
- `Known-issues registry validation`: enforces that tracked Tier-2 or waived Tier-1 breakages include category, owner, and ETA.

## Release sign-off checklist

Before tagging a release:

1. Confirm the latest `main` run of `Platform Matrix Gate` is green.
2. Confirm all Tier-1 host jobs are passing.
3. Confirm x86-64, AArch64, and RV64GC artifact-generation checks are passing.
4. Review `docs/platform_known_issues.json`; no Tier-1 issue may remain open without an explicit owner, ETA, and release-manager acceptance.
5. Link the green workflow run and known-issues review in the release notes.

## Known-issues categories

Use these categories in `docs/platform_known_issues.json`:

- `host`: host OS or toolchain availability issue.
- `target`: target crate or target triple generation issue.
- `abi`: calling convention, object format, unwind, or relocation issue.
- `toolchain`: Rust, LLVM, linker, debugger, or platform SDK issue.

Tier-2 issues are allowed to be open when visible and owned. Tier-1 issues require a deliberate release-manager waiver and must not silently pass.
