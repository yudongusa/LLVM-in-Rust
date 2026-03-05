; NOTE: This fixture is parse-only (no llvm-as validation).
; Our type system uses a single opaque ptr type with no addrspace support.
; The roundtrip produces "addrspacecast ptr %p to ptr" which is a valid
; parse target but is rejected by llvm-as (same address space).
define ptr @addrspacecast_noop(ptr %p) {
entry:
  %r = addrspacecast ptr %p to ptr
  ret ptr %r
}
