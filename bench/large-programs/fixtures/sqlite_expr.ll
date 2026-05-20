; Hand-written LLVM IR representing SQLite's expression evaluator hot path.
; Exercises chained comparisons, arithmetic on integers, and multiple branches.
; Mirrors the structure of sqlite3VdbeExec()'s OP_Add/OP_Compare dispatch.

target triple = "x86_64-unknown-linux-gnu"

; Helper: compute absolute value of an i64
define i64 @sqlite_abs(i64 %x) {
entry:
  %neg = icmp slt i64 %x, 0
  br i1 %neg, label %negate, label %done

negate:
  %r = sub i64 0, %x
  br label %done

done:
  %result = phi i64 [ %x, %entry ], [ %r, %negate ]
  ret i64 %result
}

; Evaluate a simple arithmetic expression: ((a + b) * c - d) / e
; Returns 0 on division-by-zero (sqlite3 convention).
define i64 @sqlite_eval_expr(i64 %a, i64 %b, i64 %c, i64 %d, i64 %e) {
entry:
  %zero_check = icmp eq i64 %e, 0
  br i1 %zero_check, label %div_zero, label %compute

compute:
  %sum   = add  i64 %a, %b
  %prod  = mul  i64 %sum, %c
  %diff  = sub  i64 %prod, %d
  %quot  = sdiv i64 %diff, %e
  ret i64 %quot

div_zero:
  ret i64 0
}

; Compare two rows lexicographically across 4 integer columns.
; Returns -1 / 0 / +1 (sqlite3_vdbeRecordCompare pattern).
define i32 @sqlite_row_compare(ptr %lhs, ptr %rhs) {
entry:
  %l0p = getelementptr i64, ptr %lhs, i64 0
  %r0p = getelementptr i64, ptr %rhs, i64 0
  %l0  = load i64, ptr %l0p, align 8
  %r0  = load i64, ptr %r0p, align 8
  %c0  = icmp eq i64 %l0, %r0
  br i1 %c0, label %col1, label %cmp0

cmp0:
  %lt0 = icmp slt i64 %l0, %r0
  br i1 %lt0, label %ret_neg, label %ret_pos

col1:
  %l1p = getelementptr i64, ptr %lhs, i64 1
  %r1p = getelementptr i64, ptr %rhs, i64 1
  %l1  = load i64, ptr %l1p, align 8
  %r1  = load i64, ptr %r1p, align 8
  %c1  = icmp eq i64 %l1, %r1
  br i1 %c1, label %col2, label %cmp1

cmp1:
  %lt1 = icmp slt i64 %l1, %r1
  br i1 %lt1, label %ret_neg, label %ret_pos

col2:
  %l2p = getelementptr i64, ptr %lhs, i64 2
  %r2p = getelementptr i64, ptr %rhs, i64 2
  %l2  = load i64, ptr %l2p, align 8
  %r2  = load i64, ptr %r2p, align 8
  %c2  = icmp eq i64 %l2, %r2
  br i1 %c2, label %col3, label %cmp2

cmp2:
  %lt2 = icmp slt i64 %l2, %r2
  br i1 %lt2, label %ret_neg, label %ret_pos

col3:
  %l3p = getelementptr i64, ptr %lhs, i64 3
  %r3p = getelementptr i64, ptr %rhs, i64 3
  %l3  = load i64, ptr %l3p, align 8
  %r3  = load i64, ptr %r3p, align 8
  %c3  = icmp eq i64 %l3, %r3
  br i1 %c3, label %ret_zero, label %cmp3

cmp3:
  %lt3 = icmp slt i64 %l3, %r3
  br i1 %lt3, label %ret_neg, label %ret_pos

ret_neg:
  ret i32 -1

ret_zero:
  ret i32 0

ret_pos:
  ret i32 1
}

; Aggregate sum over an integer array (sqlite3 SUM aggregate function kernel).
define i64 @sqlite_sum(ptr %arr, i32 %n) {
entry:
  %n64 = sext i32 %n to i64
  br label %loop

loop:
  %i   = phi i64 [ 0,    %entry ], [ %i1,  %loop ]
  %acc = phi i64 [ 0,    %entry ], [ %acc1, %loop ]
  %cmp = icmp slt i64 %i, %n64
  br i1 %cmp, label %body, label %exit

body:
  %p   = getelementptr i64, ptr %arr, i64 %i
  %v   = load i64, ptr %p, align 8
  %acc1 = add i64 %acc, %v
  %i1  = add i64 %i, 1
  br label %loop

exit:
  ret i64 %acc
}
