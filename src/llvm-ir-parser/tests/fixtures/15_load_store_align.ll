define i64 @load_aligned(ptr %p) {
entry:
  %v = load i64, ptr %p, align 8
  ret i64 %v
}
define void @store_aligned(ptr %p, i64 %v) {
entry:
  store i64 %v, ptr %p, align 8
  ret void
}
