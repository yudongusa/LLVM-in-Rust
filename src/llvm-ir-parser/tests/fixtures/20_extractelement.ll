define i32 @extract0(<4 x i32> %v) {
entry:
  %r = extractelement <4 x i32> %v, i32 0
  ret i32 %r
}
define i32 @extract_dyn(<4 x i32> %v, i32 %idx) {
entry:
  %r = extractelement <4 x i32> %v, i32 %idx
  ret i32 %r
}
