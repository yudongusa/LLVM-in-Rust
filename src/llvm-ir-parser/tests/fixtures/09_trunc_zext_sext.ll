define i8 @trunc_i64_i8(i64 %x) {
entry:
  %r = trunc i64 %x to i8
  ret i8 %r
}
define i32 @trunc_i64_i32(i64 %x) {
entry:
  %r = trunc i64 %x to i32
  ret i32 %r
}
define i64 @zext_i8_i64(i8 %x) {
entry:
  %r = zext i8 %x to i64
  ret i64 %r
}
define i64 @zext_i32_i64(i32 %x) {
entry:
  %r = zext i32 %x to i64
  ret i64 %r
}
define i64 @sext_i8_i64(i8 %x) {
entry:
  %r = sext i8 %x to i64
  ret i64 %r
}
define i64 @sext_i32_i64(i32 %x) {
entry:
  %r = sext i32 %x to i64
  ret i64 %r
}
