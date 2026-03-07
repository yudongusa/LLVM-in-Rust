# Issue #83 Checklist

- [ ] `/// # Correctness` section added in `mem2reg.rs`.
- [ ] At least 20 before/after Alive2 `.ll` pairs added under `tests/alive2/mem2reg/`.
- [ ] Property-based test generates >= 500 random alloca patterns.
- [ ] Semantic equivalence checked by running both original and mem2reg outputs through x86 codegen + linker.
- [ ] Full tests pass.
- [ ] PR review comment posted before merge.
