define i64 @fptoui_d(double %x) {
entry:
  %r = fptoui double %x to i64
  ret i64 %r
}
define i64 @fptosi_d(double %x) {
entry:
  %r = fptosi double %x to i64
  ret i64 %r
}
define double @uitofp_i64(i64 %x) {
entry:
  %r = uitofp i64 %x to double
  ret double %r
}
define double @sitofp_i64(i64 %x) {
entry:
  %r = sitofp i64 %x to double
  ret double %r
}
