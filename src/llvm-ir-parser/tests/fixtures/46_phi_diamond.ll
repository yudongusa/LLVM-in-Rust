define i32 @diamond(i1 %c, i32 %a, i32 %b) {
entry:
  br i1 %c, label %left, label %right
left:
  %la = add i32 %a, 1
  br label %merge
right:
  %rb = sub i32 %b, 1
  br label %merge
merge:
  %v = phi i32 [ %la, %left ], [ %rb, %right ]
  ret i32 %v
}
