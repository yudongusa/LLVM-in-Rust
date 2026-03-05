define i32 @fill_and_sum() {
entry:
  %arr = alloca [4 x i32]
  %p0 = getelementptr inbounds [4 x i32], ptr %arr, i32 0, i32 0
  store i32 10, ptr %p0
  %p1 = getelementptr inbounds [4 x i32], ptr %arr, i32 0, i32 1
  store i32 20, ptr %p1
  %p2 = getelementptr inbounds [4 x i32], ptr %arr, i32 0, i32 2
  store i32 30, ptr %p2
  %p3 = getelementptr inbounds [4 x i32], ptr %arr, i32 0, i32 3
  store i32 40, ptr %p3
  %v0 = load i32, ptr %p0
  %v1 = load i32, ptr %p1
  %v2 = load i32, ptr %p2
  %v3 = load i32, ptr %p3
  %s0 = add i32 %v0, %v1
  %s1 = add i32 %s0, %v2
  %s2 = add i32 %s1, %v3
  ret i32 %s2
}
