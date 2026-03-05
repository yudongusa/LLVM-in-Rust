%Point = type { i32, i32 }
%Seg = type { %Point, %Point }
define i32 @seg_dx(%Seg %s) {
entry:
  %p0 = extractvalue %Seg %s, 0
  %p1 = extractvalue %Seg %s, 1
  %x0 = extractvalue %Point %p0, 0
  %x1 = extractvalue %Point %p1, 0
  %dx = sub i32 %x1, %x0
  ret i32 %dx
}
