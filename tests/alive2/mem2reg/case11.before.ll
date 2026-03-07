; mem2reg before case 11
define i32 @main() {
entry:
  %p = alloca i32
  store i32 11, ptr %p
  %v = load i32, ptr %p
  ret i32 %v
}
