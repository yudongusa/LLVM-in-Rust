define i32 @clamp(i32 %x, i32 %lo, i32 %hi) {
entry:
  %cmp_lo = icmp slt i32 %x, %lo
  %t0 = select i1 %cmp_lo, i32 %lo, i32 %x
  %cmp_hi = icmp sgt i32 %t0, %hi
  %t1 = select i1 %cmp_hi, i32 %hi, i32 %t0
  ret i32 %t1
}
