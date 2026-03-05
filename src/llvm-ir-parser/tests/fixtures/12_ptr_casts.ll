define i64 @ptrtoint_op(ptr %p) {
entry:
  %r = ptrtoint ptr %p to i64
  ret i64 %r
}
define ptr @inttoptr_op(i64 %x) {
entry:
  %r = inttoptr i64 %x to ptr
  ret ptr %r
}
