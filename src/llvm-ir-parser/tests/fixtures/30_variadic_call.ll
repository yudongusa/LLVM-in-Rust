declare i32 @printf(ptr, ...)
define i32 @use_printf(ptr %fmt, i32 %x) {
entry:
  %r = call i32 @printf(ptr %fmt, i32 %x)
  ret i32 %r
}
