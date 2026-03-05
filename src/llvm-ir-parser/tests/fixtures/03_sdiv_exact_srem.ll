define i64 @sdiv_exact(i64 %a, i64 %b) {
entry:
  %r = sdiv exact i64 %a, %b
  ret i64 %r
}
define i64 @srem_op(i64 %a, i64 %b) {
entry:
  %r = srem i64 %a, %b
  ret i64 %r
}
