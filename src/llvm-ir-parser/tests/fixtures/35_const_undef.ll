define i32 @undef_phi(i1 %c) {
entry:
  br i1 %c, label %left, label %right
left:
  br label %merge
right:
  br label %merge
merge:
  %v = phi i32 [ undef, %left ], [ 42, %right ]
  ret i32 %v
}
define i32 @undef_select(i1 %c) {
entry:
  %v = select i1 %c, i32 undef, i32 0
  ret i32 %v
}
