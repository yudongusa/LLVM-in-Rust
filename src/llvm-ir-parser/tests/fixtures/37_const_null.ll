define i1 @is_null(ptr %p) {
entry:
  %r = icmp eq ptr %p, null
  ret i1 %r
}
define ptr @return_null() {
entry:
  ret ptr null
}
