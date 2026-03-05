define float @fptrunc_to_f32(double %x) {
entry:
  %r = fptrunc double %x to float
  ret float %r
}
define double @fpext_to_f64(float %x) {
entry:
  %r = fpext float %x to double
  ret double %r
}
