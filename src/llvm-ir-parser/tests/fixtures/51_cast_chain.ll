define i8 @int_cast_chain(i8 %x) {
entry:
  %a = sext i8 %x to i64
  %b = sitofp i64 %a to double
  %c = fptoui double %b to i64
  %d = trunc i64 %c to i8
  ret i8 %d
}
define ptr @ptr_round_trip(ptr %p) {
entry:
  %i = ptrtoint ptr %p to i64
  %q = inttoptr i64 %i to ptr
  ret ptr %q
}
