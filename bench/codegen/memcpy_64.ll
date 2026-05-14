target triple = "x86_64-unknown-linux-gnu"

; 64-byte copy-shaped loop. Memory ops are intentionally present even while
; this backend still lowers scalar memory traffic conservatively.
define i32 @main(ptr %dst, ptr %src) {
entry:
  %i = alloca i64, align 8
  store i64 0, ptr %i, align 8
  br label %loop
loop:
  %iv = load i64, ptr %i, align 8
  %sp = getelementptr i8, ptr %src, i64 %iv
  %dp = getelementptr i8, ptr %dst, i64 %iv
  %v = load i8, ptr %sp, align 1
  store i8 %v, ptr %dp, align 1
  %next = add i64 %iv, 1
  store i64 %next, ptr %i, align 8
  %done = icmp ult i64 %next, 64
  br i1 %done, label %loop, label %exit
exit:
  ret i32 0
}
