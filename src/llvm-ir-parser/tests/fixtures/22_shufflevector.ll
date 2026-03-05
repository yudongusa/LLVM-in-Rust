define <4 x i32> @shuffle_identity(<4 x i32> %a, <4 x i32> %b) {
entry:
  %r = shufflevector <4 x i32> %a, <4 x i32> %b, <4 x i32> <i32 0, i32 1, i32 2, i32 3>
  ret <4 x i32> %r
}
define <4 x i32> @shuffle_reverse(<4 x i32> %a, <4 x i32> %b) {
entry:
  %r = shufflevector <4 x i32> %a, <4 x i32> %b, <4 x i32> <i32 3, i32 2, i32 1, i32 0>
  ret <4 x i32> %r
}
