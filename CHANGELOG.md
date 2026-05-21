# Changelog

All notable changes to LLVM-in-Rust are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/). This project is currently in the `0.x.y` release series; see the README Versioning section for API compatibility expectations.

## [Unreleased]

### Added

- Release-readiness documentation for the first public crate release.
- A SemVer policy covering the pre-1.0 series, the 1.0 readiness bar, and post-1.0 compatibility rules.

### Milestone L — API & documentation polish

- Fixed all clippy warnings workspace-wide (zero warnings under `-D warnings`).
- Added `#[allow(clippy::too_many_arguments)]` to `build_inline_asm`, `lower_instr`, and `emit_terminator` to preserve public signatures.
- Replaced manual `div_ceil`, `is_multiple_of`, `clamp`, and `repeat_n` patterns with their standard-library equivalents (Rust 1.73+).
- Derived `Default` for `Linkage` via `#[derive(Default)]` + `#[default]` on the `External` variant.
- Improved rustdoc coverage: `#[allow(missing_docs)]` on `InstrKind` and `ConstantData` variant fields; brief doc comments added to all other undocumented public items.
- Added `//! # Examples` doc-tests (`no_run`) to `llvm-ir`, `llvm-transforms`, and `llvm-bitcode` crate roots.
- Added `hello_world.rs` and `opt_pipeline.rs` examples to the `llvm-in-rust` crate (`src/llvm/examples/`).
- Added a "Quick Start" section to `README.md` with a 30-line end-to-end builder + optimizer snippet.

## [0.1.0] - Unreleased

Initial public release candidate for the safe-Rust LLVM pipeline.

### Added

#### Phase 1 — IR foundation

- Core IR model for modules, functions, basic blocks, instructions, types, values, and constants.
- Builder APIs for constructing IR programmatically.
- LLVM-like text printer and `.ll` parser support for the implemented IR subset.
- Foundational round-trip coverage for constructing, printing, and parsing IR.

#### Phase 2 — Analysis infrastructure

- Control-flow graph construction and reachability queries.
- Dominator tree analysis using the Lengauer-Tarjan algorithm.
- Use-def chain analysis for instruction and value users.
- Natural loop detection with documented reducibility assumptions.

#### Phase 3 — Transform pipeline

- Scalar promotion with mem2reg.
- Constant folding and constant propagation passes.
- Dead-code elimination.
- Function inlining and pass-manager infrastructure.
- Optimization preset pipelines for `-O0`, `-O1`, `-O2`, and `-O3`.
- Dedicated constant-folding pass integration for `-O1` and above.

#### Phase 4 — x86_64 backend

- Target-independent machine IR, instruction selection, legalization, register allocation, scheduling, and object emission scaffolding.
- x86_64 instruction selection and lowering for integer, memory, branch, call, and selected vector operations.
- Linear-scan register allocation, later supplemented by a graph-coloring allocator and allocation strategy switch.
- ELF and Mach-O object emission.
- Pattern-based instruction selection, including multiply-by-constant lowering.
- Deterministic golden-codegen corpus gate.

#### Phase 5 — AArch64 backend and LRIR

- AArch64 instruction selection, lowering, register handling, and object-emission coverage.
- Custom LRIR binary serialization format with reader and writer support.
- Round-trip validation across text IR and LRIR paths.

#### Phase 6 — Debug, unwind, and production gates

- IR metadata round-trip preservation.
- Backend debug-location propagation into object emission.
- DWARF line-table baseline, DWARF 5 DIE emission, and `.debug_loclists` support.
- ELF/COFF unwind metadata, `.eh_frame`, `.xdata/.pdata`, and frame-aware unwind validation.
- Windows COFF/CodeView debug-info milestone.
- Linker/debugger/ABI interoperability conformance suite.
- Bootstrap compatibility matrix, platform support policy, sanitizer/UB hardening gate, crash-triage automation, and production operations documentation.

#### Phase 7 — SIMD and floating-point expansion

- SSE4.2, AVX2, and AVX-512F vector lowering paths with feature and width gating.
- Auto-vectorization support for implemented vector IR shapes.
- Floating-point and SIMD regression coverage across parser, transforms, and backend lowering.

#### Phase 8 — LTO baseline

- Embedded IR payload support in generated objects.
- Link-time payload recovery and cross-module optimization helpers.
- Top-level `llvm::lto` facade for LTO-ready flows.

#### Tooling and release engineering

- Continuous differential testing against LLVM tools.
- Parser fuzzing with LLVM-Stress and CSmith inputs.
- Formal/property-oriented mem2reg verification corpus.
- Performance budget gate and benchmark documentation.
- Reproducible release artifact provenance pipeline and release-candidate protocol.
- RISC-V RV64GC backend implementation and validation coverage.

### Fixed

#### Phase 3 correctness fixes

- #17 — Fixed constant-fold shift masks for integer types narrower than `i64`.
- #18 — Fixed signed constant-folding operations for narrow integer types by applying the required sign extension.
- #19 — Fixed inliner callee lookup to resolve functions from the correct module storage.
- #20 — Fixed cloned instruction IDs in the inliner by offsetting into the caller instruction pool.
- #21 — Fixed cloned basic-block IDs in the inliner by offsetting into the caller block pool.
- #22 — Fixed constant propagation so phi values are not missed by a single forward scan over out-of-order or cyclic blocks.
- #23 — Fixed call-result substitution after inlining so uses in downstream multi-block regions are updated.

#### Phase 4 correctness fixes

- #29 — Fixed allocation rewriting for destination registers in machine instructions.
- #30 — Fixed physical-register moves that were being emitted as NOPs.
- #31 — Fixed unsigned division and remainder lowering to use unsigned `DIV` rather than signed `IDIV`.
- #32 — Fixed conditional-branch phi destruction so successor-specific copies do not execute unconditionally.
- #33 — Fixed shift lowering to preserve and encode the shift-amount operand.
- #34 — Fixed binary-operation encoding so source-register selection does not alias the destination incorrectly.
- #35 — Fixed `SETCC` encoding for RSI/RDI byte registers by emitting the required REX prefix.
- #36 — Fixed sign-extension lowering to select the correct source width instead of always using 32-to-64 `MOVSXD`.
- #37 — Fixed Mach-O string tables to start with the required null byte.
- #38 — Fixed linear-scan active-set maintenance to avoid unnecessary repeated sorting.

#### Phase 5 correctness fixes

- #49 — Fixed metadata type interning so `TypeData::Metadata` maps to metadata rather than label type.
- #50 — Fixed AArch64 wide-immediate materialization for 64-bit values above bit 31.
- #51 — Fixed select lowering of all-ones masks.
- #52 — Fixed AArch64 memory-operation lowering so alloca/load/store/GEP do not leave uninitialized live virtual registers.

[Unreleased]: https://github.com/yudongusa/LLVM-in-Rust/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/yudongusa/LLVM-in-Rust/releases/tag/v0.1.0
