%S = type { i32, i64 }
@zero_s = global %S zeroinitializer
define void @init_zero(ptr %p) {
entry:
  store %S zeroinitializer, ptr %p
  ret void
}
