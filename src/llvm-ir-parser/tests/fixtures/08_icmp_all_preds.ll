define i1 @icmp_eq(i32 %a, i32 %b) {
entry:
  %r = icmp eq i32 %a, %b
  ret i1 %r
}
define i1 @icmp_ne(i32 %a, i32 %b) {
entry:
  %r = icmp ne i32 %a, %b
  ret i1 %r
}
define i1 @icmp_ugt(i32 %a, i32 %b) {
entry:
  %r = icmp ugt i32 %a, %b
  ret i1 %r
}
define i1 @icmp_uge(i32 %a, i32 %b) {
entry:
  %r = icmp uge i32 %a, %b
  ret i1 %r
}
define i1 @icmp_ult(i32 %a, i32 %b) {
entry:
  %r = icmp ult i32 %a, %b
  ret i1 %r
}
define i1 @icmp_ule(i32 %a, i32 %b) {
entry:
  %r = icmp ule i32 %a, %b
  ret i1 %r
}
define i1 @icmp_sgt(i32 %a, i32 %b) {
entry:
  %r = icmp sgt i32 %a, %b
  ret i1 %r
}
define i1 @icmp_sge(i32 %a, i32 %b) {
entry:
  %r = icmp sge i32 %a, %b
  ret i1 %r
}
define i1 @icmp_slt(i32 %a, i32 %b) {
entry:
  %r = icmp slt i32 %a, %b
  ret i1 %r
}
define i1 @icmp_sle(i32 %a, i32 %b) {
entry:
  %r = icmp sle i32 %a, %b
  ret i1 %r
}
