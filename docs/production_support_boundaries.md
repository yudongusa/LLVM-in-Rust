# Production Support Boundaries

This document is the public support contract for the pre-1.0 series. It
defines the scoped production-ready contract, keeps that contract distinct from
general LLVM replacement readiness, and records unsupported deployment modes.

## Production Scope

| Scenario | Status | Requirements and limits |
|---|---|---|
| Scoped fallback-backed production workload | Production-ready with controls | Trusted LLVM 15+ opaque-pointer IR; pinned LLVM-in-Rust commit or release; target/backend selected from the matrix below; release-blocking CI green on that commit; fallback to upstream LLVM documented and exercised by the embedding project. |
| General drop-in replacement for LLVM | Not supported | LLVM IR, backend, runtime, and platform coverage are broad but not complete. The Milestone Z pilot validated a fallback-backed posture, not standalone compiler replacement readiness. |
| Untrusted or adversarial IR/object input | Not supported without sandboxing | Run parser, optimizer, codegen, and JIT paths in a separate process/container with CPU, memory, file-system, and wall-clock limits. Disable JIT for hostile input unless the host already has a hardened executable-memory policy. See `docs/sandbox_deployment.md`. |
| Research, benchmarking, and compiler experiments | Supported | APIs and behavior may change across `0.x` minor releases; pin revisions and record validation commands when publishing results. |

## Pre-1.0 API Stability Matrix

| Surface | Stability | Breaking-change policy |
|---|---|---|
| Core IR data model (`llvm-ir` types, values, modules, builder, printer) | Stable enough for pinned pilot use | Patch releases avoid intentional breaks. `0.x` minor releases may rename fields, variants, or builders when needed for correctness; migration notes go in the changelog. |
| LLVM text parser and printer (`llvm-ir-parser`, `Printer`) | Stable enough for the documented LLVM 15+ subset | Accepted syntax may expand in patch/minor releases. Rejections for unsupported constructs are not considered API breaks. |
| Pass manager and public transforms (`llvm-transforms`) | Experimental | Pass ordering, analysis invalidation, and optimization behavior may change in any `0.x` minor release. |
| Codegen, backend, object emission, and JIT crates | Experimental | Machine IR, relocation handling, register allocation, and executable-memory behavior can change while backend support contracts are finalized. |
| LRIR binary format and LLVM bitstream reader | Experimental | Format versions may change before 1.0. Persist text `.ll` when long-term compatibility is required. |
| rustc backend, Wasm backend, sanitizer passes, PGO/LTO helpers | Experimental | Usable for smoke tests and pilots only where the corresponding support table cell says "pilot" or "experimental". |
| Internal crates, scripts, fixtures, and golden baselines | Internal | No compatibility promise. These can change whenever CI, minimization, or benchmark infrastructure changes. |

## Backend and Platform Boundaries

| Area | Current production contract | Known limits |
|---|---|---|
| x86-64 native backend | Pilot-supported for trusted IR through the tested integer, FP/SIMD, atomics, calls, object emission, debug, unwind, LTO, and JIT paths. SysV and Win64 ABI coverage are both represented in tests. | Not a general LLVM `-O2` quality replacement. Inline asm constraints, exotic relocations, and cross-language EH runtime behavior remain scoped/experimental. |
| AArch64 backend | Pilot-supported for trusted IR object generation and tested integer, FP/SIMD, atomics, calls, debug, unwind metadata, and LTO payload paths. | Runtime execution coverage is narrower than x86-64. Full platform-linker and language EH interop remain scoped by the backend support matrix. |
| RISC-V RV64GC backend | Experimental pilot support for artifact generation, integer/FP calling convention subsets, atomics, object emission, and backend tests. | Runtime linker/execution coverage and lowering quality paths remain pilot/experimental where the backend support matrix marks them so. |
| WebAssembly backend | Experimental. Can emit wasm modules for simple functions and supported control-flow shapes. | Arbitrary CFG/relooper completeness, stack-frame allocation for `alloca`, loop phi destruction, indirect calls, external imports, FP/vector/atomic breadth, and host execution contracts are not production-supported yet. |
| JIT execution engine | Experimental x86-64 pilot surface for trusted IR in controlled hosts. | Uses executable memory. Do not expose to untrusted input without process isolation and host-level memory-execution policy. Keep JIT smoke and differential tests green for each scoped production use. |
| rustc backend | Experimental proof-of-concept/staged backend driver. Stable-compatible shim tests and nightly lanes exist. | Real `rustc_private`/`rustc_codegen_ssa` integration and nightly-driver support are not a general production backend contract. |
| LTO | Pilot-supported for embedding and recovering LRIR payloads in ELF/COFF (`.llvmbc`, `.llvmcmd`) and Mach-O (`__LLVM,__bitcode`, `__LLVM,__cmdline`) objects. | LRIR is project-specific and not LLVM bitcode. Cross-toolchain LTO compatibility with upstream LLVM is not supported. |
| Debug and unwind | Pilot-supported metadata/object sections include DWARF line/info/loclist paths, ELF `.eh_frame`, COFF `.pdata`/`.xdata`, and CodeView `.debug$S` where implemented. | Unwind data is not a full language-runtime EH guarantee. Mach-O unwind parity and cross-language throw/catch interop remain experimental. |
| ELF | Pilot-supported for the primary native-object paths and CI/tool-backed checks. | Unsupported or target-specific relocations must be documented as known issues before release sign-off. |
| Mach-O | Pilot-supported for object emission and selected debug/LTO paths. | Unwind parity is narrower than ELF/COFF, and platform execution coverage must be explicitly checked for each pilot. |
| COFF | Pilot-supported for x86-64/Windows object emission, Win64 ABI hardening, CodeView/PDB milestones, `.pdata`/`.xdata`, and Windows CI paths. | Full SEH funclet runtime parity and non-x86 Windows targets remain outside the current production contract. |
| Known unsupported cases | Unsupported unless a linked issue or known-issue entry says otherwise | LLVM <=14 typed-pointer IR, arbitrary unsupported intrinsics, unrestricted inline asm, hostile inputs without sandboxing, unsupported relocation/linker combinations, and production use of any matrix cell marked experimental. |

## Untrusted Input Deployment

The detailed sandboxing runbook is
[`docs/sandbox_deployment.md`](sandbox_deployment.md). It covers process and
container isolation, Linux seccomp/cgroups, macOS `sandbox-exec`, Windows job
objects, resource limits, temporary-directory hygiene, and JIT disablement for
hostile inputs.

## Documentation Truth Policy

- README status language must stay scoped: "production-ready" means scoped,
  fallback-backed production use with controls, not general LLVM replacement.
- Hard-coded test counts are avoided in public status text. CI and the validation
  commands in `docs/production_operations.md` are the current quality signal.
- Feature tables must distinguish IR parse/round-trip support from backend
  lowering, runtime execution, and platform-linker support.
- Each production-readiness milestone in #93 should close only after its PRs are
  merged, tests are green, and the roadmap issue is updated with the final status.
