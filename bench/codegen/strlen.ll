target triple = "x86_64-unknown-linux-gnu"

; Null-terminated string-length loop with an eight-byte guard to keep the
; benchmark finite and deterministic for object-only codegen checks.
define i32 @main(ptr %s) {
entry:
  %i = alloca i64, align 8
  store i64 0, ptr %i, align 8
  br label %loop
loop:
  %iv = load i64, ptr %i, align 8
  %p = getelementptr i8, ptr %s, i64 %iv
  %ch = load i8, ptr %p, align 1
  %is_zero = icmp eq i8 %ch, 0
  br i1 %is_zero, label %exit, label %cont
cont:
  %next = add i64 %iv, 1
  store i64 %next, ptr %i, align 8
  %under_limit = icmp ult i64 %next, 8
  br i1 %under_limit, label %loop, label %exit
exit:
  %rv = load i64, ptr %i, align 8
  %r = trunc i64 %rv to i32
  ret i32 %r
}
