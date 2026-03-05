define <4 x i32> @vec_add(<4 x i32> %a, <4 x i32> %b) {
entry:
  %r = add <4 x i32> %a, %b
  ret <4 x i32> %r
}
define <4 x float> @vec_fmul(<4 x float> %a, <4 x float> %b) {
entry:
  %r = fmul <4 x float> %a, %b
  ret <4 x float> %r
}
