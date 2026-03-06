//! RISC-V machine opcode constants used by lowering and encoder.

use llvm_codegen::isel::MOpcode;

pub const NOP: MOpcode = MOpcode(0x00);
pub const MOV_RR: MOpcode = MOpcode(0x01);
pub const MOV_IMM: MOpcode = MOpcode(0x02);
pub const MOV_PR: MOpcode = MOpcode(0x03);

pub const ADD_RR: MOpcode = MOpcode(0x10);
pub const SUB_RR: MOpcode = MOpcode(0x11);
pub const MUL_RR: MOpcode = MOpcode(0x12);
pub const DIV_RR: MOpcode = MOpcode(0x13);
pub const UDIV_RR: MOpcode = MOpcode(0x14);
pub const REM_RR: MOpcode = MOpcode(0x15);
pub const UREM_RR: MOpcode = MOpcode(0x16);

pub const AND_RR: MOpcode = MOpcode(0x20);
pub const OR_RR: MOpcode = MOpcode(0x21);
pub const XOR_RR: MOpcode = MOpcode(0x22);
pub const SLL_RR: MOpcode = MOpcode(0x23);
pub const SRL_RR: MOpcode = MOpcode(0x24);
pub const SRA_RR: MOpcode = MOpcode(0x25);
pub const SLT_RR: MOpcode = MOpcode(0x26);
pub const SLTU_RR: MOpcode = MOpcode(0x27);

pub const ADDI: MOpcode = MOpcode(0x30);
pub const XORI: MOpcode = MOpcode(0x31);
pub const SLTIU: MOpcode = MOpcode(0x32);

pub const LW: MOpcode = MOpcode(0x40);
pub const LD: MOpcode = MOpcode(0x41);
pub const SW: MOpcode = MOpcode(0x42);
pub const SD: MOpcode = MOpcode(0x43);

pub const BEQ: MOpcode = MOpcode(0x50);
pub const BNE: MOpcode = MOpcode(0x51);
pub const BLT: MOpcode = MOpcode(0x52);
pub const BGE: MOpcode = MOpcode(0x53);
pub const BLTU: MOpcode = MOpcode(0x54);
pub const BGEU: MOpcode = MOpcode(0x55);

pub const JAL: MOpcode = MOpcode(0x60);
pub const JALR: MOpcode = MOpcode(0x61);
pub const RET: MOpcode = MOpcode(0x62);

pub const LUI: MOpcode = MOpcode(0x70);
pub const AUIPC: MOpcode = MOpcode(0x71);
