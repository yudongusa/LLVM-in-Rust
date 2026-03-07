# Alive2 Fixtures

This directory stores offline verification fixtures for optimization passes.

## Mem2Reg corpus

- Location: `tests/alive2/mem2reg/`
- Files: `caseNN.before.ll` and `caseNN.after.ll` (`NN = 01..20`)

Each pair captures a concrete `mem2reg` rewrite target and its expected SSA form.

To verify all pairs with Alive2:

```bash
skills/mem2reg-verification/scripts/verify_alive2_pairs.sh
```

This script requires `alive-tv` from the Alive2 toolchain to be installed.
