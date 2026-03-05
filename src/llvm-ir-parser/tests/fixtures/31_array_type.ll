define i32 @array_sum() {
entry:
  %arr = alloca [4 x i32]
  %p0 = getelementptr inbounds [4 x i32], ptr %arr, i64 0, i64 0
  store i32 10, ptr %p0
  %p1 = getelementptr inbounds [4 x i32], ptr %arr, i64 0, i64 1
  store i32 20, ptr %p1
  %v0 = load i32, ptr %p0
  %v1 = load i32, ptr %p1
  %r = add i32 %v0, %v1
  ret i32 %r
}
