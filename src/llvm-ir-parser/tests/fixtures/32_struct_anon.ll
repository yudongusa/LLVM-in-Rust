define i32 @get_field({ i32, double } %s) {
entry:
  %v = extractvalue { i32, double } %s, 0
  ret i32 %v
}
