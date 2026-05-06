# Interoperability Conformance Suite (M2 Gate)

This suite converts the existing compatibility checks into a maintained conformance gate for linker, debugger, ABI, and mixed-toolchain behavior.

## CI gate

The `Interoperability Conformance Gate` workflow runs on PRs, pushes to `main`, weekly schedule, and manual dispatch. It is split into actionable failure categories:

| Lane | Category | Coverage |
|---|---|---|
| `link` | `link` | `ld`/`lld`/`cc` availability, object visibility, symbol table, and host link smoke through `llvm-codegen` linker compatibility tests |
| `debug` | `debug` | DWARF line mapping, CodeView/COFF metadata checks, and debugger-tool presence reporting for `gdb`/`lldb` |
| `abi` | `abi` | x86-64 System V/Windows x64, AArch64 AAPCS64, RV64GC backend checks, and unwind object compatibility |
| `mixed` | `mixed` | LLVM 19 toolchain smoke, parser differential roundtrip, bitcode library checks, and Rust toolchain interop |

Each lane is invoked through `scripts/interoperability_conformance.sh <lane>`, which emits GitHub Actions log groups prefixed as `[conformance:<category>]` so failures can be routed directly to link/debug/ABI/mixed owners.

## Local usage

```bash
scripts/interoperability_conformance.sh link
scripts/interoperability_conformance.sh debug
scripts/interoperability_conformance.sh abi
scripts/interoperability_conformance.sh mixed
scripts/interoperability_conformance.sh release
```

The local runner expects Rust stable and whichever external tools are relevant for the lane. CI installs LLVM 19, `lld`, `gdb`, `lldb`, and binutils before executing the suite.

## Release sign-off

Before release:

1. Confirm the latest `main` run of `Interoperability Conformance Gate` is green.
2. Confirm any failure is categorized as `link`, `debug`, `abi`, or `mixed` in the workflow logs.
3. Confirm no known linker/debugger/ABI exception is undocumented in the relevant M2 issue or release notes.
4. Link the green workflow run in the release checklist.

## Expansion policy

Add new conformance coverage by extending the script with a lane/category first, then wiring CI. Prefer small, categorized lanes over one large opaque job so regressions remain actionable.
