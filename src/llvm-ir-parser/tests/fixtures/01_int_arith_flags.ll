define i32 @add_nuw(i32 %a, i32 %b) {
entry:
  %r = add nuw i32 %a, %b
  ret i32 %r
}
define i32 @add_nsw(i32 %a, i32 %b) {
entry:
  %r = add nsw i32 %a, %b
  ret i32 %r
}
define i32 @sub_nuw(i32 %a, i32 %b) {
entry:
  %r = sub nuw i32 %a, %b
  ret i32 %r
}
define i32 @mul_nsw(i32 %a, i32 %b) {
entry:
  %r = mul nsw i32 %a, %b
  ret i32 %r
}
