define double @fadd_fast(double %a, double %b) {
entry:
  %r = fadd fast double %a, %b
  ret double %r
}
define double @fsub_nnan_ninf(double %a, double %b) {
entry:
  %r = fsub nnan ninf double %a, %b
  ret double %r
}
define double @fmul_nsz(double %a, double %b) {
entry:
  %r = fmul nsz double %a, %b
  ret double %r
}
