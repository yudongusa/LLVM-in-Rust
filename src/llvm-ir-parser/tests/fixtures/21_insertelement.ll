define <4 x i32> @insert0(<4 x i32> %v, i32 %x) {
entry:
  %r = insertelement <4 x i32> %v, i32 %x, i32 0
  ret <4 x i32> %r
}
define <4 x float> @insert_f32(<4 x float> %v, float %x, i32 %idx) {
entry:
  %r = insertelement <4 x float> %v, float %x, i32 %idx
  ret <4 x float> %r
}
