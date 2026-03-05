define i32 @add(i32 %a, i32 %b) {
entry:
  %r = add i32 %a, %b
  ret i32 %r
}
define i32 @mul(i32 %a, i32 %b) {
entry:
  %r = mul i32 %a, %b
  ret i32 %r
}
define i32 @fma(i32 %a, i32 %b, i32 %c) {
entry:
  %ab = call i32 @mul(i32 %a, i32 %b)
  %r = call i32 @add(i32 %ab, i32 %c)
  ret i32 %r
}
