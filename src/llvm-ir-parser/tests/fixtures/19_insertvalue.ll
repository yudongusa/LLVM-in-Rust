%KV = type { i32, i64 }
define %KV @set_key(%KV %p, i32 %v) {
entry:
  %r = insertvalue %KV %p, i32 %v, 0
  ret %KV %r
}
define %KV @set_val(%KV %p, i64 %v) {
entry:
  %r = insertvalue %KV %p, i64 %v, 1
  ret %KV %r
}
