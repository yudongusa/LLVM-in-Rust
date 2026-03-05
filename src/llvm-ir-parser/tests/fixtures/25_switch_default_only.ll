define i32 @only_default(i32 %x) {
entry:
  switch i32 %x, label %def [
  ]
def:
  ret i32 0
}
