%Rect = type { i32, i32 }
define i32 @get_x(%Rect %r) {
entry:
  %x = extractvalue %Rect %r, 0
  ret i32 %x
}
define i32 @get_y(%Rect %r) {
entry:
  %y = extractvalue %Rect %r, 1
  ret i32 %y
}
