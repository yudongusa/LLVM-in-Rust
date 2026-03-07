# Issue #92 Execution Checklist

- [ ] Parse `!dbg` attachments and `!DILocation` metadata records.
- [ ] Thread debug metadata signal into codegen.
- [ ] Emit `.debug_line` for debug-carrying modules.
- [ ] Add integration test for debug-line section generation.
- [ ] Validate using `readelf`/`llvm-dwarfdump` when available.
- [ ] Run full test suite.
- [ ] Post PR review comment with findings and fixes.
