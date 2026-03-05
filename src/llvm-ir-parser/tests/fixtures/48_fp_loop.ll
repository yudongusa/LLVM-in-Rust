define double @fp_accum(double %a, double %b, double %c) {
entry:
  %t0 = fadd double %a, %b
  %t1 = fmul double %t0, %c
  %t2 = fsub double %t1, %a
  %t3 = fdiv double %t2, %b
  ret double %t3
}
