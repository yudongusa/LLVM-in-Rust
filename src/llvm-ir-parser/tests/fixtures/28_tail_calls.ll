declare i32 @helper(i32)
define i32 @tail_caller(i32 %x) {
entry:
  %r = tail call i32 @helper(i32 %x)
  ret i32 %r
}
define i32 @notail_caller(i32 %x) {
entry:
  %r = notail call i32 @helper(i32 %x)
  ret i32 %r
}
