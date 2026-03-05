define void @multi_phi(i32 %a, i32 %b, i1 %flag) {
entry:
  br i1 %flag, label %left, label %right
left:
  br label %merge
right:
  br label %merge
merge:
  %x = phi i32 [ %b, %left ], [ %a, %right ]
  %y = phi i32 [ %a, %left ], [ %b, %right ]
  ret void
}
