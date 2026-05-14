target triple = "x86_64-unknown-linux-gnu"

; Eight-element integer dot product.
define i32 @main(ptr %a, ptr %b) {
entry:
  %i = alloca i64, align 8
  %sum = alloca i32, align 4
  store i64 0, ptr %i, align 8
  store i32 0, ptr %sum, align 4
  br label %loop
loop:
  %iv = load i64, ptr %i, align 8
  %sv = load i32, ptr %sum, align 4
  %ap = getelementptr i32, ptr %a, i64 %iv
  %bp = getelementptr i32, ptr %b, i64 %iv
  %av = load i32, ptr %ap, align 4
  %bv = load i32, ptr %bp, align 4
  %prod = mul i32 %av, %bv
  %sum2 = add i32 %sv, %prod
  store i32 %sum2, ptr %sum, align 4
  %next = add i64 %iv, 1
  store i64 %next, ptr %i, align 8
  %done = icmp ult i64 %next, 8
  br i1 %done, label %loop, label %exit
exit:
  %result = load i32, ptr %sum, align 4
  ret i32 %result
}
