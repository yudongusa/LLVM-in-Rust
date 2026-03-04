//! x86_64 machine opcode constants and condition-code values.

use llvm_codegen::isel::MOpcode;

// ── data movement ──────────────────────────────────────────────────────────
/// `mov dst, src`   (64-bit reg → reg)
pub const MOV_RR:    MOpcode = MOpcode(0x00);
/// `mov dst, imm64` (64-bit immediate)
pub const MOV_RI:    MOpcode = MOpcode(0x01);
/// Sign-extend 32-bit source to 64-bit destination (`movsxd`)
pub const MOVSX_32:  MOpcode = MOpcode(0x02);
/// Sign-extend 8-bit source to 64-bit destination (`movsx`)
pub const MOVSX_8:   MOpcode = MOpcode(0x03);
/// Zero-extend 8-bit source to 64-bit destination (`movzx`)
pub const MOVZX_8:   MOpcode = MOpcode(0x04);
/// Sign-extend 16-bit source to 64-bit destination (`movsx`)
pub const MOVSX_16:  MOpcode = MOpcode(0x06);
/// Move VReg source into a fixed physical register destination.
/// Layout: `operands[0]` = `PReg` (destination, ABI-fixed), `operands[1]` = `VReg`/`PReg` (source).
/// Used by `emit_mov_to_preg`; unlike `MOV_RR` there is no `dst` field so the
/// physical register in `operands[0]` survives register allocation unchanged.
pub const MOV_PR:    MOpcode = MOpcode(0x05);

// ── integer arithmetic ─────────────────────────────────────────────────────
pub const ADD_RR:    MOpcode = MOpcode(0x10);
pub const ADD_RI:    MOpcode = MOpcode(0x11);
pub const SUB_RR:    MOpcode = MOpcode(0x12);
pub const SUB_RI:    MOpcode = MOpcode(0x13);
/// `imul dst, src`  (2-operand: dst *= src)
pub const IMUL_RR:   MOpcode = MOpcode(0x14);
/// `imul dst, src, imm`  (3-operand)
pub const IMUL_RRI:  MOpcode = MOpcode(0x15);
/// `idiv src`  (signed: rdx:rax ÷ src → rax=quot, rdx=rem; requires CQO first)
pub const IDIV_R:    MOpcode = MOpcode(0x16);
/// `neg dst`
pub const NEG_R:     MOpcode = MOpcode(0x17);
/// `cqo` — sign-extend rax into rdx:rax before idiv
pub const CQO:       MOpcode = MOpcode(0x18);
/// `div src`  (unsigned: rdx:rax ÷ src → rax=quot, rdx=rem; requires `xor rdx, rdx` first)
pub const DIV_R:     MOpcode = MOpcode(0x19);

// ── bitwise ────────────────────────────────────────────────────────────────
pub const AND_RR:    MOpcode = MOpcode(0x20);
pub const AND_RI:    MOpcode = MOpcode(0x21);
pub const OR_RR:     MOpcode = MOpcode(0x22);
pub const OR_RI:     MOpcode = MOpcode(0x23);
pub const XOR_RR:    MOpcode = MOpcode(0x24);
pub const XOR_RI:    MOpcode = MOpcode(0x25);
pub const NOT_R:     MOpcode = MOpcode(0x26);

// ── shifts ─────────────────────────────────────────────────────────────────
/// `shl dst, cl`  (logical left shift by register)
pub const SHL_RR:    MOpcode = MOpcode(0x30);
/// `shl dst, imm8`
pub const SHL_RI:    MOpcode = MOpcode(0x31);
/// `shr dst, cl`  (logical right shift by register)
pub const SHR_RR:    MOpcode = MOpcode(0x32);
/// `shr dst, imm8`
pub const SHR_RI:    MOpcode = MOpcode(0x33);
/// `sar dst, cl`  (arithmetic right shift by register)
pub const SAR_RR:    MOpcode = MOpcode(0x34);
/// `sar dst, imm8`
pub const SAR_RI:    MOpcode = MOpcode(0x35);

// ── comparisons ────────────────────────────────────────────────────────────
pub const CMP_RR:    MOpcode = MOpcode(0x40);
pub const CMP_RI:    MOpcode = MOpcode(0x41);
pub const TEST_RR:   MOpcode = MOpcode(0x42);
/// `setcc dst`  — condition code stored as `Imm(CC_*)` in first operand.
pub const SETCC:     MOpcode = MOpcode(0x43);

// ── control flow ───────────────────────────────────────────────────────────
pub const JMP:          MOpcode = MOpcode(0x50);
/// `jcc target`  — condition code stored as `Imm(CC_*)`, target as `Block`.
pub const JCC:          MOpcode = MOpcode(0x51);
/// `call rel32`
pub const CALL_DIRECT:  MOpcode = MOpcode(0x52);
/// `call *reg`
pub const CALL_R:       MOpcode = MOpcode(0x53);
pub const RET:          MOpcode = MOpcode(0x54);

// ── stack ──────────────────────────────────────────────────────────────────
pub const PUSH_R:    MOpcode = MOpcode(0x60);
pub const POP_R:     MOpcode = MOpcode(0x61);

// ── miscellaneous ──────────────────────────────────────────────────────────
pub const NOP:       MOpcode = MOpcode(0x70);
/// `lea dst, [base + imm]`  — imm stored as `Imm` operand.
pub const LEA_RI:    MOpcode = MOpcode(0x71);

// ── condition codes (used as Imm operands with JCC / SETCC) ────────────────
pub const CC_EQ:  i64 = 0;  // je  / jz
pub const CC_NE:  i64 = 1;  // jne / jnz
pub const CC_LT:  i64 = 2;  // jl  (signed)
pub const CC_LE:  i64 = 3;  // jle (signed)
pub const CC_GT:  i64 = 4;  // jg  (signed)
pub const CC_GE:  i64 = 5;  // jge (signed)
pub const CC_ULT: i64 = 6;  // jb  (unsigned below)
pub const CC_ULE: i64 = 7;  // jbe (unsigned below-or-equal)
pub const CC_UGT: i64 = 8;  // ja  (unsigned above)
pub const CC_UGE: i64 = 9;  // jae (unsigned above-or-equal)
