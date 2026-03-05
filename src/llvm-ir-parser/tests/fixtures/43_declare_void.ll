declare void @free(ptr)
define void @call_free(ptr %p) {
entry:
  call void @free(ptr %p)
  ret void
}
