define i1 @fcmp_oeq(double %a, double %b) {
entry:
  %r = fcmp oeq double %a, %b
  ret i1 %r
}
define i1 @fcmp_ogt(double %a, double %b) {
entry:
  %r = fcmp ogt double %a, %b
  ret i1 %r
}
define i1 @fcmp_oge(double %a, double %b) {
entry:
  %r = fcmp oge double %a, %b
  ret i1 %r
}
define i1 @fcmp_olt(double %a, double %b) {
entry:
  %r = fcmp olt double %a, %b
  ret i1 %r
}
define i1 @fcmp_ole(double %a, double %b) {
entry:
  %r = fcmp ole double %a, %b
  ret i1 %r
}
define i1 @fcmp_one(double %a, double %b) {
entry:
  %r = fcmp one double %a, %b
  ret i1 %r
}
define i1 @fcmp_ord(double %a, double %b) {
entry:
  %r = fcmp ord double %a, %b
  ret i1 %r
}
define i1 @fcmp_uno(double %a, double %b) {
entry:
  %r = fcmp uno double %a, %b
  ret i1 %r
}
define i1 @fcmp_ueq(double %a, double %b) {
entry:
  %r = fcmp ueq double %a, %b
  ret i1 %r
}
define i1 @fcmp_ugt(double %a, double %b) {
entry:
  %r = fcmp ugt double %a, %b
  ret i1 %r
}
define i1 @fcmp_uge(double %a, double %b) {
entry:
  %r = fcmp uge double %a, %b
  ret i1 %r
}
define i1 @fcmp_ult(double %a, double %b) {
entry:
  %r = fcmp ult double %a, %b
  ret i1 %r
}
define i1 @fcmp_ule(double %a, double %b) {
entry:
  %r = fcmp ule double %a, %b
  ret i1 %r
}
define i1 @fcmp_une(double %a, double %b) {
entry:
  %r = fcmp une double %a, %b
  ret i1 %r
}
define i1 @fcmp_true(double %a, double %b) {
entry:
  %r = fcmp true double %a, %b
  ret i1 %r
}
define i1 @fcmp_false(double %a, double %b) {
entry:
  %r = fcmp false double %a, %b
  ret i1 %r
}
