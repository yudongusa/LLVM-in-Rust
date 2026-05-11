//! x86_64 machine opcode constants and condition-code values.

use llvm_codegen::isel::MOpcode;

// ── data movement ──────────────────────────────────────────────────────────
/// `mov dst, src`   (64-bit reg → reg)
pub const MOV_RR: MOpcode = MOpcode(0x00);
/// `mov dst, imm64` (64-bit immediate)
pub const MOV_RI: MOpcode = MOpcode(0x01);
/// Sign-extend 32-bit source to 64-bit destination (`movsxd`)
pub const MOVSX_32: MOpcode = MOpcode(0x02);
/// Sign-extend 8-bit source to 64-bit destination (`movsx`)
pub const MOVSX_8: MOpcode = MOpcode(0x03);
/// Zero-extend 8-bit source to 64-bit destination (`movzx`)
pub const MOVZX_8: MOpcode = MOpcode(0x04);
/// Sign-extend 16-bit source to 64-bit destination (`movsx`)
pub const MOVSX_16: MOpcode = MOpcode(0x06);
/// Move VReg source into a fixed physical register destination.
/// Layout: `operands[0]` = `PReg` (destination, ABI-fixed), `operands[1]` = `VReg`/`PReg` (source).
/// Used by `emit_mov_to_preg`; unlike `MOV_RR` there is no `dst` field so the
/// physical register in `operands[0]` survives register allocation unchanged.
pub const MOV_PR: MOpcode = MOpcode(0x05);

// ── integer arithmetic ─────────────────────────────────────────────────────
/// Public API for `ADD_RR`.
pub const ADD_RR: MOpcode = MOpcode(0x10);
/// Public API for `ADD_RI`.
pub const ADD_RI: MOpcode = MOpcode(0x11);
/// Public API for `SUB_RR`.
pub const SUB_RR: MOpcode = MOpcode(0x12);
/// Public API for `SUB_RI`.
pub const SUB_RI: MOpcode = MOpcode(0x13);
/// `imul dst, src`  (2-operand: dst *= src)
pub const IMUL_RR: MOpcode = MOpcode(0x14);
/// `imul dst, src, imm`  (3-operand)
pub const IMUL_RRI: MOpcode = MOpcode(0x15);
/// `idiv src`  (signed: rdx:rax ÷ src → rax=quot, rdx=rem; requires CQO first)
pub const IDIV_R: MOpcode = MOpcode(0x16);
/// `neg dst`
pub const NEG_R: MOpcode = MOpcode(0x17);
/// `cqo` — sign-extend rax into rdx:rax before idiv
pub const CQO: MOpcode = MOpcode(0x18);
/// `div src`  (unsigned: rdx:rax ÷ src → rax=quot, rdx=rem; requires `xor rdx, rdx` first)
pub const DIV_R: MOpcode = MOpcode(0x19);

// ── bitwise ────────────────────────────────────────────────────────────────
/// Public API for `AND_RR`.
pub const AND_RR: MOpcode = MOpcode(0x20);
/// Public API for `AND_RI`.
pub const AND_RI: MOpcode = MOpcode(0x21);
/// Public API for `OR_RR`.
pub const OR_RR: MOpcode = MOpcode(0x22);
/// Public API for `OR_RI`.
pub const OR_RI: MOpcode = MOpcode(0x23);
/// Public API for `XOR_RR`.
pub const XOR_RR: MOpcode = MOpcode(0x24);
/// Public API for `XOR_RI`.
pub const XOR_RI: MOpcode = MOpcode(0x25);
/// Public API for `NOT_R`.
pub const NOT_R: MOpcode = MOpcode(0x26);

// ── shifts ─────────────────────────────────────────────────────────────────
/// `shl dst, cl`  (logical left shift by register)
pub const SHL_RR: MOpcode = MOpcode(0x30);
/// `shl dst, imm8`
pub const SHL_RI: MOpcode = MOpcode(0x31);
/// `shr dst, cl`  (logical right shift by register)
pub const SHR_RR: MOpcode = MOpcode(0x32);
/// `shr dst, imm8`
pub const SHR_RI: MOpcode = MOpcode(0x33);
/// `sar dst, cl`  (arithmetic right shift by register)
pub const SAR_RR: MOpcode = MOpcode(0x34);
/// `sar dst, imm8`
pub const SAR_RI: MOpcode = MOpcode(0x35);

// ── comparisons ────────────────────────────────────────────────────────────
/// Public API for `CMP_RR`.
pub const CMP_RR: MOpcode = MOpcode(0x40);
/// Public API for `CMP_RI`.
pub const CMP_RI: MOpcode = MOpcode(0x41);
/// Public API for `TEST_RR`.
pub const TEST_RR: MOpcode = MOpcode(0x42);
/// `setcc dst`  — condition code stored as `Imm(CC_*)` in first operand.
pub const SETCC: MOpcode = MOpcode(0x43);

// ── control flow ───────────────────────────────────────────────────────────
/// Public API for `JMP`.
pub const JMP: MOpcode = MOpcode(0x50);
/// `jcc target`  — condition code stored as `Imm(CC_*)`, target as `Block`.
pub const JCC: MOpcode = MOpcode(0x51);
/// `call rel32`
pub const CALL_DIRECT: MOpcode = MOpcode(0x52);
/// `call *reg`
pub const CALL_R: MOpcode = MOpcode(0x53);
/// Public API for `RET`.
pub const RET: MOpcode = MOpcode(0x54);

// ── stack ──────────────────────────────────────────────────────────────────
/// Public API for `PUSH_R`.
pub const PUSH_R: MOpcode = MOpcode(0x60);
/// Public API for `POP_R`.
pub const POP_R: MOpcode = MOpcode(0x61);

// ── miscellaneous ──────────────────────────────────────────────────────────
/// Public API for `NOP`.
pub const NOP: MOpcode = MOpcode(0x70);
/// `lea dst, [base + imm]`  — imm stored as `Imm` operand.
pub const LEA_RI: MOpcode = MOpcode(0x71);

// ── frame (spill) access ───────────────────────────────────────────────────
/// `mov dst, [rbp + disp]`  — spill reload.  `dst` = VReg, `operands[0]` = `Imm(slot)`.
pub const MOV_LOAD_MR: MOpcode = MOpcode(0x72);
/// `mov [rbp + disp], src`  — spill store.  `dst` = None, `operands[0]` = `Imm(slot)`, `operands[1]` = VReg/PReg(src).
pub const MOV_STORE_RM: MOpcode = MOpcode(0x73);

// ── SIMD / vector (SSE4.2 baseline for vector lowering) ───────────────────
/// Public API for `PADDD_RR`.
pub const PADDD_RR: MOpcode = MOpcode(0x80);
/// Public API for `PSUBD_RR`.
pub const PSUBD_RR: MOpcode = MOpcode(0x81);
/// Public API for `PMULLD_RR`.
pub const PMULLD_RR: MOpcode = MOpcode(0x82);
/// Public API for `ADDPS_RR`.
pub const ADDPS_RR: MOpcode = MOpcode(0x83);
/// Public API for `MULPS_RR`.
pub const MULPS_RR: MOpcode = MOpcode(0x84);
/// Public API for `DIVPS_RR`.
pub const DIVPS_RR: MOpcode = MOpcode(0x85);
/// Public API for `ADDPD_RR`.
pub const ADDPD_RR: MOpcode = MOpcode(0x86);
/// Public API for `MULPD_RR`.
pub const MULPD_RR: MOpcode = MOpcode(0x87);
/// Public API for `MOVAPS_RR`.
pub const MOVAPS_RR: MOpcode = MOpcode(0x88);
/// Public API for `MOVDQU_LOAD_MR`.
pub const MOVDQU_LOAD_MR: MOpcode = MOpcode(0x89);
/// Public API for `MOVDQU_STORE_RM`.
pub const MOVDQU_STORE_RM: MOpcode = MOpcode(0x8A);
/// Public API for `MOVAPS_LOAD_MR`.
pub const MOVAPS_LOAD_MR: MOpcode = MOpcode(0x8B);

// ── condition codes (used as Imm operands with JCC / SETCC) ────────────────
/// Public API for `CC_EQ`.
pub const CC_EQ: i64 = 0; // je  / jz
/// Public API for `CC_NE`.
pub const CC_NE: i64 = 1; // jne / jnz
/// Public API for `CC_LT`.
pub const CC_LT: i64 = 2; // jl  (signed)
/// Public API for `CC_LE`.
pub const CC_LE: i64 = 3; // jle (signed)
/// Public API for `CC_GT`.
pub const CC_GT: i64 = 4; // jg  (signed)
/// Public API for `CC_GE`.
pub const CC_GE: i64 = 5; // jge (signed)
/// Public API for `CC_ULT`.
pub const CC_ULT: i64 = 6; // jb  (unsigned below)
/// Public API for `CC_ULE`.
pub const CC_ULE: i64 = 7; // jbe (unsigned below-or-equal)
/// Public API for `CC_UGT`.
pub const CC_UGT: i64 = 8; // ja  (unsigned above)
/// Public API for `CC_UGE`.
pub const CC_UGE: i64 = 9; // jae (unsigned above-or-equal)
