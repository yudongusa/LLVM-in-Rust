//! RISC-V machine opcode constants used by lowering and encoder.

use llvm_codegen::isel::MOpcode;

/// Public API for `NOP`.
pub const NOP: MOpcode = MOpcode(0x00);
/// Public API for `MOV_RR`.
pub const MOV_RR: MOpcode = MOpcode(0x01);
/// Public API for `MOV_IMM`.
pub const MOV_IMM: MOpcode = MOpcode(0x02);
/// Public API for `MOV_PR`.
pub const MOV_PR: MOpcode = MOpcode(0x03);

/// Public API for `ADD_RR`.
pub const ADD_RR: MOpcode = MOpcode(0x10);
/// Public API for `SUB_RR`.
pub const SUB_RR: MOpcode = MOpcode(0x11);
/// Public API for `MUL_RR`.
pub const MUL_RR: MOpcode = MOpcode(0x12);
/// Public API for `DIV_RR`.
pub const DIV_RR: MOpcode = MOpcode(0x13);
/// Public API for `UDIV_RR`.
pub const UDIV_RR: MOpcode = MOpcode(0x14);
/// Public API for `REM_RR`.
pub const REM_RR: MOpcode = MOpcode(0x15);
/// Public API for `UREM_RR`.
pub const UREM_RR: MOpcode = MOpcode(0x16);

/// Public API for `AND_RR`.
pub const AND_RR: MOpcode = MOpcode(0x20);
/// Public API for `OR_RR`.
pub const OR_RR: MOpcode = MOpcode(0x21);
/// Public API for `XOR_RR`.
pub const XOR_RR: MOpcode = MOpcode(0x22);
/// Public API for `SLL_RR`.
pub const SLL_RR: MOpcode = MOpcode(0x23);
/// Public API for `SRL_RR`.
pub const SRL_RR: MOpcode = MOpcode(0x24);
/// Public API for `SRA_RR`.
pub const SRA_RR: MOpcode = MOpcode(0x25);
/// Public API for `SLT_RR`.
pub const SLT_RR: MOpcode = MOpcode(0x26);
/// Public API for `SLTU_RR`.
pub const SLTU_RR: MOpcode = MOpcode(0x27);

/// Public API for `ADDI`.
pub const ADDI: MOpcode = MOpcode(0x30);
/// Public API for `XORI`.
pub const XORI: MOpcode = MOpcode(0x31);
/// Public API for `SLTIU`.
pub const SLTIU: MOpcode = MOpcode(0x32);

/// Public API for `LW`.
pub const LW: MOpcode = MOpcode(0x40);
/// Public API for `LD`.
pub const LD: MOpcode = MOpcode(0x41);
/// Public API for `SW`.
pub const SW: MOpcode = MOpcode(0x42);
/// Public API for `SD`.
pub const SD: MOpcode = MOpcode(0x43);

/// Public API for `BEQ`.
pub const BEQ: MOpcode = MOpcode(0x50);
/// Public API for `BNE`.
pub const BNE: MOpcode = MOpcode(0x51);
/// Public API for `BLT`.
pub const BLT: MOpcode = MOpcode(0x52);
/// Public API for `BGE`.
pub const BGE: MOpcode = MOpcode(0x53);
/// Public API for `BLTU`.
pub const BLTU: MOpcode = MOpcode(0x54);
/// Public API for `BGEU`.
pub const BGEU: MOpcode = MOpcode(0x55);

/// Public API for `JAL`.
pub const JAL: MOpcode = MOpcode(0x60);
/// Public API for `JALR`.
pub const JALR: MOpcode = MOpcode(0x61);
/// Public API for `RET`.
pub const RET: MOpcode = MOpcode(0x62);

/// Public API for `LUI`.
pub const LUI: MOpcode = MOpcode(0x70);
/// Public API for `AUIPC`.
pub const AUIPC: MOpcode = MOpcode(0x71);
