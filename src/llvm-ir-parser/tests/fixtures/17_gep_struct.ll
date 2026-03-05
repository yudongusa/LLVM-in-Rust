%Pair = type { i32, i64 }
define ptr @gep_field0(ptr %s) {
entry:
  %p = getelementptr inbounds %Pair, ptr %s, i32 0, i32 0
  ret ptr %p
}
define ptr @gep_field1(ptr %s) {
entry:
  %p = getelementptr inbounds %Pair, ptr %s, i32 0, i32 1
  ret ptr %p
}
