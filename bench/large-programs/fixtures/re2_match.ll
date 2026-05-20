; Hand-written LLVM IR representing RE2's NFA state machine hot path.
; Exercises a state-dispatch loop with phi nodes, select, and comparisons.
; Mirrors the structure of re2's NFA::Search / ByteRange matching inner loop.

target triple = "x86_64-unknown-linux-gnu"

; Advance one NFA state given an input byte.
; state:  current state index (0..N)
; byte:   current input byte
; table:  flat transition table — table[state * 256 + byte] = next_state
; Returns next state, or -1 if no transition.
define i32 @re2_nfa_step(i32 %state, i8 %byte, ptr %table) {
entry:
  %b32    = zext  i8  %byte to i32
  %b64    = sext  i32 %b32  to i64
  %s64    = sext  i32 %state to i64
  %row    = mul   i64 %s64, 256
  %col    = add   i64 %row, %b64
  %tp     = getelementptr i32, ptr %table, i64 %col
  %next   = load  i32, ptr %tp, align 4
  ret i32 %next
}

; Run the NFA over a byte string, looking for a match.
; buf:     input string
; len:     length of input
; table:   NFA transition table (n_states * 256 i32 entries)
; accept:  accepting state id (match if current state == accept at any point)
; Returns index of first accepting position, or -1 if no match.
define i32 @re2_search(ptr %buf, i32 %len, ptr %table, i32 %accept) {
entry:
  %len64 = sext i32 %len to i64
  br label %loop

loop:
  %i     = phi i64 [ 0, %entry ], [ %i1,      %cont ]
  %state = phi i32 [ 0, %entry ], [ %state1,  %cont ]
  %cmp   = icmp ult i64 %i, %len64
  br i1 %cmp, label %step, label %no_match

step:
  %p      = getelementptr i8, ptr %buf, i64 %i
  %byte   = load i8, ptr %p, align 1
  %b32    = zext i8 %byte to i32
  %b64    = sext i32 %b32  to i64
  %s64    = sext i32 %state to i64
  %row    = mul  i64 %s64, 256
  %col    = add  i64 %row, %b64
  %tp     = getelementptr i32, ptr %table, i64 %col
  %state1 = load i32, ptr %tp, align 4
  %dead   = icmp eq i32 %state1, -1
  br i1 %dead, label %no_match, label %check_accept

check_accept:
  %matched = icmp eq i32 %state1, %accept
  br i1 %matched, label %found, label %cont

cont:
  %i1 = add i64 %i, 1
  br label %loop

found:
  %pos = trunc i64 %i to i32
  ret i32 %pos

no_match:
  ret i32 -1
}

; Count matches of a pattern in a text string (greedy scan).
; buf:     input text
; len:     text length
; table:   NFA table
; accept:  accepting state
; Returns count of non-overlapping matches.
define i32 @re2_count_matches(ptr %buf, i32 %len, ptr %table, i32 %accept) {
entry:
  %len64 = sext i32 %len to i64
  br label %outer

outer:
  %i     = phi i64 [ 0, %entry ], [ %i_next,  %next_outer ]
  %count = phi i32 [ 0, %entry ], [ %count1,  %next_outer ]
  %cmp   = icmp ult i64 %i, %len64
  br i1 %cmp, label %inner_init, label %done

inner_init:
  br label %inner

inner:
  %j      = phi i64 [ %i,     %inner_init ], [ %j1,      %inner_cont ]
  %state  = phi i32 [ 0,      %inner_init ], [ %state_n, %inner_cont ]
  %jcmp   = icmp ult i64 %j, %len64
  br i1 %jcmp, label %inner_step, label %inner_miss

inner_step:
  %p       = getelementptr i8, ptr %buf, i64 %j
  %byte    = load i8, ptr %p, align 1
  %b32     = zext i8 %byte to i32
  %b64     = sext i32 %b32 to i64
  %s64     = sext i32 %state to i64
  %row     = mul  i64 %s64, 256
  %col     = add  i64 %row, %b64
  %tp      = getelementptr i32, ptr %table, i64 %col
  %state_n = load i32, ptr %tp, align 4
  %dead    = icmp eq i32 %state_n, -1
  br i1 %dead, label %inner_miss, label %inner_accept

inner_accept:
  %hit = icmp eq i32 %state_n, %accept
  br i1 %hit, label %match_found, label %inner_cont

inner_cont:
  %j1 = add i64 %j, 1
  br label %inner

match_found:
  %count1  = add i32 %count, 1
  %i_after = add i64 %j, 1
  br label %next_outer

inner_miss:
  %i_miss = add i64 %i, 1
  br label %next_miss

next_miss:
  br label %next_outer

next_outer:
  %count1_out = phi i32 [ %count1, %match_found ], [ %count, %next_miss ]
  %i_next     = phi i64 [ %i_after, %match_found ], [ %i_miss, %next_miss ]
  br label %outer

done:
  ret i32 %count
}
