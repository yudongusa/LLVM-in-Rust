define double @fadd_d(double %a, double %b) {
entry:
  %r = fadd double %a, %b
  ret double %r
}
define double @fsub_d(double %a, double %b) {
entry:
  %r = fsub double %a, %b
  ret double %r
}
define double @fmul_d(double %a, double %b) {
entry:
  %r = fmul double %a, %b
  ret double %r
}
define double @fdiv_d(double %a, double %b) {
entry:
  %r = fdiv double %a, %b
  ret double %r
}
define double @frem_d(double %a, double %b) {
entry:
  %r = frem double %a, %b
  ret double %r
}
define double @fneg_d(double %a) {
entry:
  %r = fneg double %a
  ret double %r
}
