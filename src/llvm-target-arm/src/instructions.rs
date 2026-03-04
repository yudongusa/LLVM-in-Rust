//! AArch64 machine opcode constants and condition-code values.

use llvm_codegen::isel::MOpcode;

// ── data movement ──────────────────────────────────────────────────────────

/// `mov xd, xn` (implemented as `orr xd, xzr, xn`)
pub const MOV_RR:   MOpcode = MOpcode(0x00);
/// `movz xd, #imm16` (16-bit zero-extending immediate)
pub const MOV_IMM:  MOpcode = MOpcode(0x01);
/// `movz xd, #lo16; movk xd, #hi16, lsl 16` (64-bit immediate via two instructions)
pub const MOV_WIDE: MOpcode = MOpcode(0x02);
/// Move VReg source into a fixed physical register destination.
/// Layout: `operands[0]` = `PReg` (destination, ABI-fixed), `operands[1]` = `VReg`/`PReg` (source).
pub const MOV_PR:   MOpcode = MOpcode(0x03);

// ── integer arithmetic ─────────────────────────────────────────────────────

/// `add xd, xn, xm`
pub const ADD_RR:   MOpcode = MOpcode(0x10);
/// `sub xd, xn, xm`
pub const SUB_RR:   MOpcode = MOpcode(0x11);
/// `mul xd, xn, xm` (= `madd xd, xn, xm, xzr`)
pub const MUL_RR:   MOpcode = MOpcode(0x12);
/// `sdiv xd, xn, xm` (signed division)
pub const SDIV_RR:  MOpcode = MOpcode(0x13);
/// `udiv xd, xn, xm` (unsigned division)
pub const UDIV_RR:  MOpcode = MOpcode(0x14);
/// `neg xd, xn` (= `sub xd, xzr, xn`)
pub const NEG_R:    MOpcode = MOpcode(0x15);

// ── bitwise ────────────────────────────────────────────────────────────────

/// `and xd, xn, xm`
pub const AND_RR:   MOpcode = MOpcode(0x20);
/// `orr xd, xn, xm`
pub const ORR_RR:   MOpcode = MOpcode(0x21);
/// `eor xd, xn, xm`
pub const EOR_RR:   MOpcode = MOpcode(0x22);
/// `lslv xd, xn, xm` (logical shift left by register)
pub const LSL_RR:   MOpcode = MOpcode(0x23);
/// `lsrv xd, xn, xm` (logical shift right by register)
pub const LSR_RR:   MOpcode = MOpcode(0x24);
/// `asrv xd, xn, xm` (arithmetic shift right by register)
pub const ASR_RR:   MOpcode = MOpcode(0x25);

// ── comparisons ────────────────────────────────────────────────────────────

/// `cmp xn, xm` (= `subs xzr, xn, xm` — sets flags, discards result)
pub const CMP_RR:   MOpcode = MOpcode(0x30);
/// `cset xd, cond` — condition code stored as `Imm(CC_*)` in first operand.
pub const CSET:     MOpcode = MOpcode(0x31);

// ── control flow ───────────────────────────────────────────────────────────

/// `b offset` — unconditional branch
pub const B:        MOpcode = MOpcode(0x40);
/// `b.cond offset` — conditional branch; condition stored as `Imm(CC_*)`.
pub const B_COND:   MOpcode = MOpcode(0x41);
/// `bl offset` — branch and link (call)
pub const BL:       MOpcode = MOpcode(0x42);
/// `blr xn` — branch and link to register (indirect call)
pub const BLR:      MOpcode = MOpcode(0x43);
/// `ret x30` — return via link register
pub const RET:      MOpcode = MOpcode(0x44);

// ── memory ─────────────────────────────────────────────────────────────────

/// `ldr xd, [xn, #off]`
pub const LDR:      MOpcode = MOpcode(0x50);
/// `str xs, [xn, #off]`
pub const STR:      MOpcode = MOpcode(0x51);

// ── sign-extension ─────────────────────────────────────────────────────────

/// `sxtw xd, wn` (sign-extend 32-bit to 64-bit)
pub const SXTW:     MOpcode = MOpcode(0x60);
/// `sxtb xd, xn` (sign-extend 8-bit to 64-bit)
pub const SXTB:     MOpcode = MOpcode(0x61);
/// `sxth xd, xn` (sign-extend 16-bit to 64-bit)
pub const SXTH:     MOpcode = MOpcode(0x62);

// ── miscellaneous ──────────────────────────────────────────────────────────

/// `nop`
pub const NOP:      MOpcode = MOpcode(0x70);

// ── condition codes (used as Imm operands with B_COND / CSET) ────────────
//
// These map to AArch64 condition codes as defined in the ISA.
pub const CC_EQ: i64 = 0;  // EQ — equal (Z=1)
pub const CC_NE: i64 = 1;  // NE — not equal (Z=0)
pub const CC_LT: i64 = 2;  // LT — signed less than
pub const CC_LE: i64 = 3;  // LE — signed less than or equal
pub const CC_GT: i64 = 4;  // GT — signed greater than
pub const CC_GE: i64 = 5;  // GE — signed greater than or equal
pub const CC_LO: i64 = 6;  // LO — unsigned lower (C=0)
pub const CC_LS: i64 = 7;  // LS — unsigned lower or same (C=0 or Z=1)
pub const CC_HI: i64 = 8;  // HI — unsigned higher (C=1 and Z=0)
pub const CC_HS: i64 = 9;  // HS — unsigned higher or same (C=1)
