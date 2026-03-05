define float @fadd_f(float %a, float %b) {
entry:
  %r = fadd float %a, %b
  ret float %r
}
define float @fmul_f(float %a, float %b) {
entry:
  %r = fmul float %a, %b
  ret float %r
}
define float @fneg_f(float %a) {
entry:
  %r = fneg float %a
  ret float %r
}
