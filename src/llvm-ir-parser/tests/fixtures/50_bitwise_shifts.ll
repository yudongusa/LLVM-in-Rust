define i64 @shifts(i64 %a, i64 %b) {
entry:
  %s0 = shl i64 %a, 3
  %s1 = lshr i64 %s0, 2
  %s2 = ashr i64 %s1, 1
  %s3 = shl i64 %s2, %b
  %s4 = lshr i64 %s3, %b
  %s5 = ashr i64 %s4, %b
  %s6 = shl nuw i64 %s5, 4
  %s7 = lshr exact i64 %s6, 4
  %s8 = ashr exact i64 %s7, 2
  ret i64 %s8
}
