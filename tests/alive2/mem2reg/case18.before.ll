; mem2reg before case 18
define i32 @main() {
entry:
  %p = alloca i32
  store i32 18, ptr %p
  %v = load i32, ptr %p
  %w = add i32 %v, 3
  ret i32 %w
}
