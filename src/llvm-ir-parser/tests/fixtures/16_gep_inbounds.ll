define ptr @gep_elem(ptr %arr, i64 %idx) {
entry:
  %p = getelementptr inbounds i32, ptr %arr, i64 %idx
  ret ptr %p
}
define ptr @gep_multi(ptr %arr, i64 %i, i64 %j) {
entry:
  %p = getelementptr inbounds [10 x i32], ptr %arr, i64 %i, i64 %j
  ret ptr %p
}
