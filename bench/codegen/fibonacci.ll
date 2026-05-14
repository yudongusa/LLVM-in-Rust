target triple = "x86_64-unknown-linux-gnu"

; Iterative fibonacci kernel used by the output code-quality gate.
define i32 @main() {
entry:
  %i = alloca i32, align 4
  %a = alloca i32, align 4
  %b = alloca i32, align 4
  store i32 0, ptr %i, align 4
  store i32 0, ptr %a, align 4
  store i32 1, ptr %b, align 4
  br label %loop
loop:
  %iv = load i32, ptr %i, align 4
  %av = load i32, ptr %a, align 4
  %bv = load i32, ptr %b, align 4
  %next = add i32 %av, %bv
  store i32 %bv, ptr %a, align 4
  store i32 %next, ptr %b, align 4
  %next_i = add i32 %iv, 1
  store i32 %next_i, ptr %i, align 4
  %done = icmp slt i32 %next_i, 10
  br i1 %done, label %loop, label %exit
exit:
  %result = load i32, ptr %b, align 4
  ret i32 %result
}
