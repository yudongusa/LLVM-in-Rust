declare ptr @malloc(i64)
define ptr @alloc_int() {
entry:
  %p = call ptr @malloc(i64 4)
  ret ptr %p
}
