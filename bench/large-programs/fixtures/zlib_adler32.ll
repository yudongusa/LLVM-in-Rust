; Hand-written LLVM IR representing zlib's Adler-32 checksum computation.
; Exercises a loop with multiply, add, and modulo operations on integers.
; Mirrors the structure of zlib adler32.c's inner update loop.

target triple = "x86_64-unknown-linux-gnu"

; Compute Adler-32 checksum over a byte buffer.
; adler: current checksum (lower 16 = s1, upper 16 = s2)
; buf:   pointer to input bytes
; len:   number of bytes
; Returns updated 32-bit Adler-32 checksum.
define i32 @adler32(i32 %adler, ptr %buf, i32 %len) {
entry:
  %s1_init = and  i32 %adler, 65535
  %s2_init = lshr i32 %adler, 16
  %len64   = sext i32 %len to i64
  br label %loop

loop:
  %i    = phi i64 [ 0,        %entry ], [ %i_next,   %body ]
  %s1   = phi i32 [ %s1_init, %entry ], [ %s1_next,  %body ]
  %s2   = phi i32 [ %s2_init, %entry ], [ %s2_next,  %body ]
  %cmp  = icmp ult i64 %i, %len64
  br i1 %cmp, label %body, label %exit

body:
  %gep     = getelementptr i8, ptr %buf, i64 %i
  %byte    = load i8, ptr %gep, align 1
  %byte32  = zext i8 %byte to i32
  %s1_add  = add  i32 %s1, %byte32
  %s1_next = urem i32 %s1_add, 65521
  %s2_add  = add  i32 %s2, %s1_next
  %s2_next = urem i32 %s2_add, 65521
  %i_next  = add  i64 %i, 1
  br label %loop

exit:
  %s2_sh  = shl i32 %s2, 16
  %result = or  i32 %s2_sh, %s1
  ret i32 %result
}

; Combine two Adler-32 checksums (adler32_combine logic).
; adler1: checksum of first segment
; adler2: checksum of second segment
; len2:   byte length of second segment
define i32 @adler32_combine(i32 %adler1, i32 %adler2, i32 %len2) {
entry:
  %BASE = add i32 0, 65521
  %s1a  = and  i32 %adler1, 65535
  %s2a  = lshr i32 %adler1, 16
  %s1b  = and  i32 %adler2, 65535
  %s2b  = lshr i32 %adler2, 16
  ; s1 = (s1a + s1b) % BASE
  %s1s  = add  i32 %s1a, %s1b
  %s1r  = urem i32 %s1s, %BASE
  ; s2 = (s2a + s2b + len2 * s1a) % BASE
  %ls1  = mul  i32 %len2, %s1a
  %s2s  = add  i32 %s2a, %s2b
  %s2l  = add  i32 %s2s, %ls1
  %s2r  = urem i32 %s2l, %BASE
  %hi   = shl  i32 %s2r, 16
  %res  = or   i32 %hi, %s1r
  ret i32 %res
}

; CRC32 lookup-based byte update (zlib crc32 single-byte step).
; Represents the inner body of zlib's crc32() loop.
define i32 @crc32_byte(i32 %crc, i8 %byte, ptr %table) {
entry:
  %b32   = zext i8 %byte to i32
  %xored = xor i32 %crc, %b32
  %idx   = and i32 %xored, 255
  %idx64 = sext i32 %idx to i64
  %tp    = getelementptr i32, ptr %table, i64 %idx64
  %entry_val = load i32, ptr %tp, align 4
  %crc_shr = lshr i32 %crc, 8
  %result  = xor  i32 %crc_shr, %entry_val
  ret i32 %result
}
