; Loop using alloca/load/store (like clang -O0) to avoid forward-reference
; phi nodes, which our single-pass parser does not support.
define i64 @sum_n(i64 %n) {
entry:
  %acc = alloca i64
  %i   = alloca i64
  store i64 0, ptr %acc
  store i64 0, ptr %i
  br label %loop
loop:
  %iv   = load i64, ptr %i
  %done = icmp sge i64 %iv, %n
  br i1 %done, label %exit, label %body
body:
  %iv2  = load i64, ptr %i
  %accv = load i64, ptr %acc
  %nacc = add i64 %accv, %iv2
  store i64 %nacc, ptr %acc
  %inc  = add i64 %iv2, 1
  store i64 %inc, ptr %i
  br label %loop
exit:
  %ret = load i64, ptr %acc
  ret i64 %ret
}
