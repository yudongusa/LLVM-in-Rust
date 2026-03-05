define ptr @alloca_with_align() {
entry:
  %p = alloca i32, align 8
  ret ptr %p
}
define ptr @alloca_n_elems(i32 %n) {
entry:
  %p = alloca i32, i32 %n
  ret ptr %p
}
define ptr @alloca_n_elems_align(i32 %n) {
entry:
  %p = alloca i64, i32 %n, align 16
  ret ptr %p
}
