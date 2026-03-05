declare void @abort()
define void @must_not_return(i1 %c) {
entry:
  br i1 %c, label %ok, label %bad
ok:
  ret void
bad:
  call void @abort()
  unreachable
}
