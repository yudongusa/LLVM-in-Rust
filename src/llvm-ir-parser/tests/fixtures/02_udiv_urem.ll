define i64 @unsigned_div(i64 %a, i64 %b) {
entry:
  %r = udiv i64 %a, %b
  ret i64 %r
}
define i64 @unsigned_div_exact(i64 %a, i64 %b) {
entry:
  %r = udiv exact i64 %a, %b
  ret i64 %r
}
define i64 @unsigned_rem(i64 %a, i64 %b) {
entry:
  %r = urem i64 %a, %b
  ret i64 %r
}
