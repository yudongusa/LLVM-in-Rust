define i32 @volatile_load(ptr %p) {
entry:
  %v = load volatile i32, ptr %p
  ret i32 %v
}
define void @volatile_store(ptr %p, i32 %v) {
entry:
  store volatile i32 %v, ptr %p
  ret void
}
