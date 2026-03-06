; Benchmark fixture — representative multi-function module.
; Mirrors clang -O0 output style: loops use alloca/store/load so that
; the mem2reg pass has real work to do.
; All pointers are opaque (LLVM 15+ syntax).

source_filename = "bench_fixture"
target triple = "x86_64-unknown-linux-gnu"

; ── integer arithmetic ────────────────────────────────────────────────────────

define i64 @fib(i64 %n) {
entry:
  %n.addr = alloca i64
  %a      = alloca i64
  %b      = alloca i64
  %c      = alloca i64
  %i      = alloca i64
  store i64 %n, ptr %n.addr
  store i64 0, ptr %a
  store i64 1, ptr %b
  store i64 2, ptr %i
  %nv  = load i64, ptr %n.addr
  %le1 = icmp sle i64 %nv, 1
  br i1 %le1, label %base, label %loop
base:
  %rv0 = load i64, ptr %n.addr
  ret i64 %rv0
loop:
  %iv   = load i64, ptr %i
  %nv2  = load i64, ptr %n.addr
  %done = icmp sgt i64 %iv, %nv2
  br i1 %done, label %exit, label %body
body:
  %av  = load i64, ptr %a
  %bv  = load i64, ptr %b
  %cv  = add i64 %av, %bv
  store i64 %cv, ptr %c
  %bv2 = load i64, ptr %b
  store i64 %bv2, ptr %a
  %cv2 = load i64, ptr %c
  store i64 %cv2, ptr %b
  %iv2 = load i64, ptr %i
  %in  = add i64 %iv2, 1
  store i64 %in, ptr %i
  br label %loop
exit:
  %ret = load i64, ptr %b
  ret i64 %ret
}

define i64 @factorial(i64 %n) {
entry:
  %n.addr = alloca i64
  %acc    = alloca i64
  %i      = alloca i64
  store i64 %n, ptr %n.addr
  store i64 1, ptr %acc
  %nv  = load i64, ptr %n.addr
  store i64 %nv, ptr %i
  br label %loop
loop:
  %iv   = load i64, ptr %i
  %done = icmp sle i64 %iv, 1
  br i1 %done, label %exit, label %body
body:
  %iv2  = load i64, ptr %i
  %accv = load i64, ptr %acc
  %prod = mul i64 %accv, %iv2
  store i64 %prod, ptr %acc
  %iv3  = load i64, ptr %i
  %dec  = sub i64 %iv3, 1
  store i64 %dec, ptr %i
  br label %loop
exit:
  %ret = load i64, ptr %acc
  ret i64 %ret
}

define i64 @gcd(i64 %a, i64 %b) {
entry:
  %x = alloca i64
  %y = alloca i64
  store i64 %a, ptr %x
  store i64 %b, ptr %y
  br label %loop
loop:
  %yv   = load i64, ptr %y
  %zero = icmp eq i64 %yv, 0
  br i1 %zero, label %exit, label %body
body:
  %xv  = load i64, ptr %x
  %yv2 = load i64, ptr %y
  %rem = srem i64 %xv, %yv2
  store i64 %yv2, ptr %x
  store i64 %rem, ptr %y
  br label %loop
exit:
  %ret = load i64, ptr %x
  ret i64 %ret
}

define i64 @pow_int(i64 %base, i64 %exp) {
entry:
  %acc = alloca i64
  %e   = alloca i64
  store i64 1, ptr %acc
  store i64 %exp, ptr %e
  br label %loop
loop:
  %ev   = load i64, ptr %e
  %done = icmp sle i64 %ev, 0
  br i1 %done, label %exit, label %body
body:
  %accv = load i64, ptr %acc
  %prod = mul i64 %accv, %base
  store i64 %prod, ptr %acc
  %ev2  = load i64, ptr %e
  %dec  = sub i64 %ev2, 1
  store i64 %dec, ptr %e
  br label %loop
exit:
  %ret = load i64, ptr %acc
  ret i64 %ret
}

; ── memory / array operations ─────────────────────────────────────────────────

define i64 @sum_array(ptr %arr, i64 %len) {
entry:
  %s = alloca i64
  %i = alloca i64
  store i64 0, ptr %s
  store i64 0, ptr %i
  br label %loop
loop:
  %iv   = load i64, ptr %i
  %done = icmp sge i64 %iv, %len
  br i1 %done, label %exit, label %body
body:
  %iv2  = load i64, ptr %i
  %ptr  = getelementptr i64, ptr %arr, i64 %iv2
  %val  = load i64, ptr %ptr
  %sv   = load i64, ptr %s
  %sum  = add i64 %sv, %val
  store i64 %sum, ptr %s
  %iv3  = load i64, ptr %i
  %inc  = add i64 %iv3, 1
  store i64 %inc, ptr %i
  br label %loop
exit:
  %ret = load i64, ptr %s
  ret i64 %ret
}

define void @fill_array(ptr %arr, i64 %len, i64 %val) {
entry:
  %i = alloca i64
  store i64 0, ptr %i
  br label %loop
loop:
  %iv   = load i64, ptr %i
  %done = icmp sge i64 %iv, %len
  br i1 %done, label %exit, label %body
body:
  %iv2 = load i64, ptr %i
  %ptr = getelementptr i64, ptr %arr, i64 %iv2
  store i64 %val, ptr %ptr
  %iv3 = load i64, ptr %i
  %inc = add i64 %iv3, 1
  store i64 %inc, ptr %i
  br label %loop
exit:
  ret void
}

define void @copy_array(ptr %dst, ptr %src, i64 %len) {
entry:
  %i = alloca i64
  store i64 0, ptr %i
  br label %loop
loop:
  %iv   = load i64, ptr %i
  %done = icmp sge i64 %iv, %len
  br i1 %done, label %exit, label %body
body:
  %iv2 = load i64, ptr %i
  %sp  = getelementptr i64, ptr %src, i64 %iv2
  %dp  = getelementptr i64, ptr %dst, i64 %iv2
  %v   = load i64, ptr %sp
  store i64 %v, ptr %dp
  %iv3 = load i64, ptr %i
  %inc = add i64 %iv3, 1
  store i64 %inc, ptr %i
  br label %loop
exit:
  ret void
}

; ── comparisons and predicates ────────────────────────────────────────────────

define i1 @is_prime(i64 %n) {
entry:
  %i = alloca i64
  %lt2 = icmp slt i64 %n, 2
  br i1 %lt2, label %ret_false, label %init
init:
  store i64 2, ptr %i
  br label %loop
loop:
  %iv  = load i64, ptr %i
  %sq  = mul i64 %iv, %iv
  %big = icmp sgt i64 %sq, %n
  br i1 %big, label %ret_true, label %check
check:
  %iv2  = load i64, ptr %i
  %r    = srem i64 %n, %iv2
  %eq0  = icmp eq i64 %r, 0
  br i1 %eq0, label %ret_false, label %cont
cont:
  %iv3 = load i64, ptr %i
  %inc = add i64 %iv3, 1
  store i64 %inc, ptr %i
  br label %loop
ret_true:
  ret i1 1
ret_false:
  ret i1 0
}

define i64 @count_primes(i64 %limit) {
entry:
  %count = alloca i64
  %i     = alloca i64
  store i64 0, ptr %count
  store i64 2, ptr %i
  br label %loop
loop:
  %iv   = load i64, ptr %i
  %done = icmp sgt i64 %iv, %limit
  br i1 %done, label %exit, label %body
body:
  %iv2  = load i64, ptr %i
  %p    = call i1 @is_prime(i64 %iv2)
  br i1 %p, label %prime_yes, label %prime_no
prime_yes:
  %cv_y  = load i64, ptr %count
  %cv2_y = add i64 %cv_y, 1
  store i64 %cv2_y, ptr %count
  br label %loop_next
prime_no:
  br label %loop_next
loop_next:
  %iv3  = load i64, ptr %i
  %in2  = add i64 %iv3, 1
  store i64 %in2, ptr %i
  br label %loop
exit:
  %ret = load i64, ptr %count
  ret i64 %ret
}

; ── floating-point ────────────────────────────────────────────────────────────

define double @dot_product(ptr %a, ptr %b, i64 %n) {
entry:
  %acc = alloca double
  %i   = alloca i64
  store double 0.0, ptr %acc
  store i64 0, ptr %i
  br label %loop
loop:
  %iv   = load i64, ptr %i
  %done = icmp sge i64 %iv, %n
  br i1 %done, label %exit, label %body
body:
  %iv2  = load i64, ptr %i
  %ap   = getelementptr double, ptr %a, i64 %iv2
  %bp   = getelementptr double, ptr %b, i64 %iv2
  %av   = load double, ptr %ap
  %bv   = load double, ptr %bp
  %p    = fmul double %av, %bv
  %accv = load double, ptr %acc
  %sum  = fadd double %accv, %p
  store double %sum, ptr %acc
  %iv3  = load i64, ptr %i
  %inc  = add i64 %iv3, 1
  store i64 %inc, ptr %i
  br label %loop
exit:
  %ret = load double, ptr %acc
  ret double %ret
}

; ── bitwise / shifts ──────────────────────────────────────────────────────────

define i64 @popcount(i64 %x) {
entry:
  %v   = alloca i64
  %cnt = alloca i64
  store i64 %x, ptr %v
  store i64 0,  ptr %cnt
  br label %loop
loop:
  %vv   = load i64, ptr %v
  %done = icmp eq i64 %vv, 0
  br i1 %done, label %exit, label %body
body:
  %vv2  = load i64, ptr %v
  %low  = and i64 %vv2, 1
  %cv   = load i64, ptr %cnt
  %cv2  = add i64 %cv, %low
  store i64 %cv2, ptr %cnt
  %vv3  = load i64, ptr %v
  %sh   = lshr i64 %vv3, 1
  store i64 %sh, ptr %v
  br label %loop
exit:
  %ret = load i64, ptr %cnt
  ret i64 %ret
}

define i64 @bit_reverse(i64 %x) {
entry:
  %v = alloca i64
  %r = alloca i64
  %i = alloca i64
  store i64 %x, ptr %v
  store i64 0,  ptr %r
  store i64 0,  ptr %i
  br label %loop
loop:
  %iv   = load i64, ptr %i
  %done = icmp eq i64 %iv, 64
  br i1 %done, label %exit, label %body
body:
  %vv  = load i64, ptr %v
  %low = and i64 %vv, 1
  %rv  = load i64, ptr %r
  %rs  = shl i64 %rv, 1
  %rv2 = or i64 %rs, %low
  store i64 %rv2, ptr %r
  %vv2 = load i64, ptr %v
  %sh  = lshr i64 %vv2, 1
  store i64 %sh, ptr %v
  %iv2 = load i64, ptr %i
  %inc = add i64 %iv2, 1
  store i64 %inc, ptr %i
  br label %loop
exit:
  %ret = load i64, ptr %r
  ret i64 %ret
}

; ── multi-block control flow ──────────────────────────────────────────────────

define i64 @classify(i64 %x) {
entry:
  %neg  = icmp slt i64 %x, 0
  br i1 %neg, label %is_neg, label %check_zero
check_zero:
  %zero = icmp eq i64 %x, 0
  br i1 %zero, label %is_zero, label %check_big
check_big:
  %big  = icmp sgt i64 %x, 1000
  br i1 %big, label %is_big, label %is_small
is_neg:
  ret i64 -1
is_zero:
  ret i64 0
is_small:
  ret i64 1
is_big:
  ret i64 2
}

define i64 @abs_val(i64 %x) {
entry:
  %neg = icmp slt i64 %x, 0
  br i1 %neg, label %negate, label %keep
negate:
  %n = sub i64 0, %x
  ret i64 %n
keep:
  ret i64 %x
}

define i64 @max3(i64 %a, i64 %b, i64 %c) {
entry:
  %ab = icmp sgt i64 %a, %b
  br i1 %ab, label %a_gt_b, label %b_ge_a
a_gt_b:
  %ac = icmp sgt i64 %a, %c
  br i1 %ac, label %ret_a, label %ret_c1
b_ge_a:
  %bc = icmp sgt i64 %b, %c
  br i1 %bc, label %ret_b, label %ret_c2
ret_a:
  ret i64 %a
ret_b:
  ret i64 %b
ret_c1:
  ret i64 %c
ret_c2:
  ret i64 %c
}

; ── redundant-expression stress (for O2 GVN effectiveness) ───────────────────

define i64 @gvn_stress(i64 %a, i64 %b) {
entry:
  %x01 = add i64 %a, %b
  %x02 = add i64 %a, %b
  %s02 = add i64 %x01, %x02
  %x03 = add i64 %a, %b
  %s03 = add i64 %s02, %x03
  %x04 = add i64 %a, %b
  %s04 = add i64 %s03, %x04
  %x05 = add i64 %a, %b
  %s05 = add i64 %s04, %x05
  %x06 = add i64 %a, %b
  %s06 = add i64 %s05, %x06
  %x07 = add i64 %a, %b
  %s07 = add i64 %s06, %x07
  %x08 = add i64 %a, %b
  %s08 = add i64 %s07, %x08
  %x09 = add i64 %a, %b
  %s09 = add i64 %s08, %x09
  %x10 = add i64 %a, %b
  %s10 = add i64 %s09, %x10
  %x11 = add i64 %a, %b
  %s11 = add i64 %s10, %x11
  %x12 = add i64 %a, %b
  %s12 = add i64 %s11, %x12
  %x13 = add i64 %a, %b
  %s13 = add i64 %s12, %x13
  %x14 = add i64 %a, %b
  %s14 = add i64 %s13, %x14
  %x15 = add i64 %a, %b
  %s15 = add i64 %s14, %x15
  %x16 = add i64 %a, %b
  %s16 = add i64 %s15, %x16
  %x17 = add i64 %a, %b
  %s17 = add i64 %s16, %x17
  %x18 = add i64 %a, %b
  %s18 = add i64 %s17, %x18
  %x19 = add i64 %a, %b
  %s19 = add i64 %s18, %x19
  %x20 = add i64 %a, %b
  %s20 = add i64 %s19, %x20
  %x21 = add i64 %a, %b
  %s21 = add i64 %s20, %x21
  %x22 = add i64 %a, %b
  %s22 = add i64 %s21, %x22
  %x23 = add i64 %a, %b
  %s23 = add i64 %s22, %x23
  %x24 = add i64 %a, %b
  %s24 = add i64 %s23, %x24
  %x25 = add i64 %a, %b
  %s25 = add i64 %s24, %x25
  %x26 = add i64 %a, %b
  %s26 = add i64 %s25, %x26
  %x27 = add i64 %a, %b
  %s27 = add i64 %s26, %x27
  %x28 = add i64 %a, %b
  %s28 = add i64 %s27, %x28
  %x29 = add i64 %a, %b
  %s29 = add i64 %s28, %x29
  %x30 = add i64 %a, %b
  %s30 = add i64 %s29, %x30
  %x31 = add i64 %a, %b
  %s31 = add i64 %s30, %x31
  %x32 = add i64 %a, %b
  %s32 = add i64 %s31, %x32
  %x33 = add i64 %a, %b
  %s33 = add i64 %s32, %x33
  %x34 = add i64 %a, %b
  %s34 = add i64 %s33, %x34
  %x35 = add i64 %a, %b
  %s35 = add i64 %s34, %x35
  %x36 = add i64 %a, %b
  %s36 = add i64 %s35, %x36
  %x37 = add i64 %a, %b
  %s37 = add i64 %s36, %x37
  %x38 = add i64 %a, %b
  %s38 = add i64 %s37, %x38
  %x39 = add i64 %a, %b
  %s39 = add i64 %s38, %x39
  %x40 = add i64 %a, %b
  %s40 = add i64 %s39, %x40
  ret i64 %s40
}
