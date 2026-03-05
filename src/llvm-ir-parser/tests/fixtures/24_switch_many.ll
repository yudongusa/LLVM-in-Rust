define i32 @day_index(i32 %day) {
entry:
  switch i32 %day, label %dflt [
    i32 1, label %mon
    i32 2, label %tue
    i32 3, label %wed
    i32 4, label %thu
    i32 5, label %fri
    i32 6, label %sat
    i32 7, label %sun
  ]
mon:
  ret i32 0
tue:
  ret i32 1
wed:
  ret i32 2
thu:
  ret i32 3
fri:
  ret i32 4
sat:
  ret i32 5
sun:
  ret i32 6
dflt:
  ret i32 -1
}
