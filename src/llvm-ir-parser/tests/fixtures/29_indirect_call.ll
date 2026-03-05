define i32 @call_fp(ptr %fp, i32 %x) {
entry:
  %r = call i32 %fp(i32 %x)
  ret i32 %r
}
