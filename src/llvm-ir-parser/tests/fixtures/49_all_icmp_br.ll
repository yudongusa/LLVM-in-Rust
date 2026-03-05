define i32 @classify(i32 %a, i32 %b) {
entry:
  %eq = icmp eq i32 %a, %b
  br i1 %eq, label %r_eq, label %check_lt
r_eq:
  ret i32 0
check_lt:
  %slt = icmp slt i32 %a, %b
  br i1 %slt, label %r_lt, label %check_ult
r_lt:
  ret i32 -1
check_ult:
  %ult = icmp ult i32 %a, %b
  br i1 %ult, label %r_ult, label %r_gt
r_ult:
  ret i32 -2
r_gt:
  ret i32 1
}
