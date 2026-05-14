target triple = "x86_64-unknown-linux-gnu"

; 4x4 integer matrix-multiply representative kernel, reduced to the first row
; dot product so the gate stays small but still exercises multiply/add chains.
define i32 @main(ptr %a, ptr %b) {
entry:
  %a0p = getelementptr i32, ptr %a, i64 0
  %a1p = getelementptr i32, ptr %a, i64 1
  %a2p = getelementptr i32, ptr %a, i64 2
  %a3p = getelementptr i32, ptr %a, i64 3
  %b0p = getelementptr i32, ptr %b, i64 0
  %b1p = getelementptr i32, ptr %b, i64 4
  %b2p = getelementptr i32, ptr %b, i64 8
  %b3p = getelementptr i32, ptr %b, i64 12
  %a0 = load i32, ptr %a0p, align 4
  %a1 = load i32, ptr %a1p, align 4
  %a2 = load i32, ptr %a2p, align 4
  %a3 = load i32, ptr %a3p, align 4
  %b0 = load i32, ptr %b0p, align 4
  %b1 = load i32, ptr %b1p, align 4
  %b2 = load i32, ptr %b2p, align 4
  %b3 = load i32, ptr %b3p, align 4
  %p0 = mul i32 %a0, %b0
  %p1 = mul i32 %a1, %b1
  %p2 = mul i32 %a2, %b2
  %p3 = mul i32 %a3, %b3
  %s0 = add i32 %p0, %p1
  %s1 = add i32 %p2, %p3
  %sum = add i32 %s0, %s1
  ret i32 %sum
}
