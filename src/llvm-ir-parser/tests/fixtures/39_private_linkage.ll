@private_ctr = private global i32 0
define private i32 @private_helper(i32 %x) {
entry:
  %r = add i32 %x, 1
  ret i32 %r
}
define i32 @public_caller(i32 %x) {
entry:
  %r = call i32 @private_helper(i32 %x)
  ret i32 %r
}
