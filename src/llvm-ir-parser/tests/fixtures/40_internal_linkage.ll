define internal i32 @internal_fn(i32 %x) {
entry:
  %r = mul i32 %x, %x
  ret i32 %r
}
define i32 @use_internal(i32 %x) {
entry:
  %r = call i32 @internal_fn(i32 %x)
  ret i32 %r
}
