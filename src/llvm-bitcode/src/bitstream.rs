//! Low-level LLVM bitstream reader.
//!
//! Implements the bitstream layer underlying the LLVM `.bc` format:
//! - VBR (Variable Bit-Rate) integer decoding
//! - Block enter/exit with abbrev-len tracking
//! - Abbreviation definition parsing (DEFINE_ABBREV records)
//! - Record reading: both unabbreviated and abbreviated forms

use crate::error::BitcodeError;

// ── Abbreviation operand encoding ─────────────────────────────────────────────

/// One operand encoding within a DEFINE_ABBREV record.
#[derive(Clone, Debug)]
pub enum AbbrevOp {
    /// A literal u64 value — the encoded field is always this fixed constant.
    Literal(u64),
    /// Fixed-width unsigned integer of `width` bits.
    Fixed(usize),
    /// Variable-bit-rate unsigned integer, group size = `width` bits.
    Vbr(usize),
    /// Array of elements; the following operand describes each element's encoding.
    Array,
    /// 6-bit character encoding (printable ASCII subset).
    Char6,
    /// Binary blob: 32-bit length then byte data (word-aligned).
    Blob,
}

/// A parsed abbreviation definition.
#[derive(Clone, Debug)]
pub struct Abbrev {
    pub ops: Vec<AbbrevOp>,
}

// ── Block metadata ─────────────────────────────────────────────────────────────

/// State saved when entering a sub-block.
struct BlockState {
    /// Abbreviation bit-width in the enclosing scope.
    abbrev_len: usize,
    /// The byte offset where the block ends (for skipping).
    end_word: usize,
}

// ── BitStreamReader ────────────────────────────────────────────────────────────

/// A cursor over a raw LLVM bitstream byte slice.
///
/// Usage:
/// 1. `check_magic` to verify `BC\xc0\xde`.
/// 2. Loop `next_abbrev_id` → `enter_block` / `read_record` / `end_block`.
pub struct BitStreamReader<'a> {
    /// Raw bytes of the bitstream (after the 4-byte magic).
    data: &'a [u8],
    /// Current bit position (into `data`).
    bit_pos: usize,
    /// Current abbreviation id width (starts at 2).
    pub abbrev_len: usize,
    /// Stack of enclosing block states (for abbrev_len restoration on exit).
    block_stack: Vec<BlockState>,
    /// User-defined abbreviations for the current block (index 0 = abbrev id 4).
    pub abbrevs: Vec<Abbrev>,
}

/// Fixed abbreviation ids (always reserved).
pub const ABBREV_END_BLOCK: u64 = 0;
pub const ABBREV_ENTER_SUBBLOCK: u64 = 1;
pub const ABBREV_DEFINE_ABBREV: u64 = 2;
pub const ABBREV_UNABBREV_RECORD: u64 = 3;

impl<'a> BitStreamReader<'a> {
    /// Create a reader positioned right after the 4-byte magic.
    pub fn new(data: &'a [u8]) -> Self {
        BitStreamReader {
            data,
            bit_pos: 0,
            abbrev_len: 2,
            block_stack: Vec::new(),
            abbrevs: Vec::new(),
        }
    }

    /// Read exactly `n` bits (0 ≤ n ≤ 64) from the stream.
    pub fn read_bits(&mut self, n: usize) -> Result<u64, BitcodeError> {
        if n == 0 {
            return Ok(0);
        }
        let mut result: u64 = 0;
        let mut bits_read = 0;
        while bits_read < n {
            let byte_idx = self.bit_pos / 8;
            if byte_idx >= self.data.len() {
                return Err(BitcodeError::UnexpectedEof);
            }
            let bit_in_byte = self.bit_pos % 8;
            let available = 8 - bit_in_byte;
            let take = (n - bits_read).min(available);
            let mask = (1u64 << take) - 1;
            let bits = ((self.data[byte_idx] as u64) >> bit_in_byte) & mask;
            result |= bits << bits_read;
            bits_read += take;
            self.bit_pos += take;
        }
        Ok(result)
    }

    /// Read a Variable Bit-Rate integer with `width`-bit groups.
    pub fn read_vbr(&mut self, width: usize) -> Result<u64, BitcodeError> {
        let low_bits = width - 1; // bits of payload per group
        let high_bit = 1u64 << low_bits; // continuation flag mask
        let payload_mask = high_bit - 1;
        let mut result: u64 = 0;
        let mut shift = 0usize;
        loop {
            let group = self.read_bits(width)?;
            result |= (group & payload_mask) << shift;
            shift += low_bits;
            if group & high_bit == 0 {
                break;
            }
        }
        Ok(result)
    }

    /// Align the bit position to the next 32-bit word boundary.
    pub fn align_32(&mut self) {
        let rem = self.bit_pos % 32;
        if rem != 0 {
            self.bit_pos += 32 - rem;
        }
    }

    /// Returns true if the reader is past the end of the data.
    pub fn is_at_end(&self) -> bool {
        self.bit_pos >= self.data.len() * 8
    }

    // ── Block management ────────────────────────────────────────────────────

    /// Read and consume an ENTER_SUBBLOCK header.
    ///
    /// Returns `(block_id, new_abbrev_len)` and enters the block's scope.
    pub fn enter_block(&mut self) -> Result<(u64, usize), BitcodeError> {
        let block_id = self.read_vbr(8)?;
        let new_abbrev_len = self.read_vbr(4)? as usize;
        self.align_32();
        // Read block length in 32-bit words (not used for navigation here,
        // but needed to skip unknown blocks).
        let block_words = self.read_bits(32)? as usize;
        let end_bit = self.bit_pos + block_words * 32;
        // Clamp to data length to avoid arithmetic overflow in edge cases.
        let end_word = end_bit.min(self.data.len() * 8);

        self.block_stack.push(BlockState {
            abbrev_len: self.abbrev_len,
            end_word,
        });
        self.abbrev_len = new_abbrev_len;
        // Clear abbreviations for the new scope.
        self.abbrevs.clear();
        Ok((block_id, new_abbrev_len))
    }

    /// Consume an END_BLOCK code: align to 32-bit boundary and restore scope.
    pub fn end_block(&mut self) -> Result<(), BitcodeError> {
        self.align_32();
        let state = self
            .block_stack
            .pop()
            .ok_or_else(|| BitcodeError::ParseError("END_BLOCK without matching block".into()))?;
        self.abbrev_len = state.abbrev_len;
        self.abbrevs.clear();
        Ok(())
    }

    /// Skip an unknown sub-block by jumping to its end_word position.
    pub fn skip_block(&mut self) -> Result<(), BitcodeError> {
        // end_word was pushed in enter_block; we need the top of the stack.
        if let Some(state) = self.block_stack.last() {
            self.bit_pos = state.end_word;
            let end = state.end_word;
            let _ = state; // borrow ends
            self.block_stack.pop();
            self.bit_pos = end;
            self.abbrevs.clear();
        }
        Ok(())
    }

    // ── Abbreviation definition ─────────────────────────────────────────────

    /// Parse a DEFINE_ABBREV record and append it to `self.abbrevs`.
    pub fn define_abbrev(&mut self) -> Result<(), BitcodeError> {
        let num_ops = self.read_vbr(5)? as usize;
        let mut ops = Vec::with_capacity(num_ops);
        let mut i = 0;
        while i < num_ops {
            let is_literal = self.read_bits(1)? != 0;
            if is_literal {
                let val = self.read_vbr(8)?;
                ops.push(AbbrevOp::Literal(val));
                i += 1;
            } else {
                let kind = self.read_bits(3)?;
                match kind {
                    1 => {
                        let width = self.read_vbr(5)? as usize;
                        ops.push(AbbrevOp::Fixed(width));
                    }
                    2 => {
                        let width = self.read_vbr(5)? as usize;
                        ops.push(AbbrevOp::Vbr(width));
                    }
                    3 => {
                        ops.push(AbbrevOp::Array);
                        // The next op describes the element encoding.
                    }
                    4 => {
                        ops.push(AbbrevOp::Char6);
                    }
                    5 => {
                        ops.push(AbbrevOp::Blob);
                    }
                    _ => {
                        return Err(BitcodeError::ParseError(format!(
                            "unknown abbrev op kind {}",
                            kind
                        )));
                    }
                }
                i += 1;
            }
        }
        self.abbrevs.push(Abbrev { ops });
        Ok(())
    }

    // ── Record reading ──────────────────────────────────────────────────────

    /// Decode the fields of a record given the already-read `abbrev_id`.
    ///
    /// This is the low-level half of record decoding.  The caller reads the
    /// abbrev_id with `read_bits(abbrev_len)` and dispatches:
    /// - `ABBREV_END_BLOCK (0)` → call `end_block()`
    /// - `ABBREV_ENTER_SUBBLOCK (1)` → call `enter_block()`
    /// - `ABBREV_DEFINE_ABBREV (2)` → call `define_abbrev()`
    /// - anything else → call `read_record_fields(abbrev_id, abbrevs)`
    ///
    /// Returns `(code, fields)` for data records.
    pub fn read_record_fields(
        &mut self,
        abbrev_id: u64,
        abbrevs: &[Abbrev],
    ) -> Result<Vec<u64>, BitcodeError> {
        let fields = match abbrev_id {
            ABBREV_UNABBREV_RECORD => {
                let code = self.read_vbr(6)?;
                let num_ops = self.read_vbr(6)? as usize;
                let mut fields = Vec::with_capacity(num_ops + 1);
                fields.push(code);
                for _ in 0..num_ops {
                    fields.push(self.read_vbr(6)?);
                }
                fields
            }
            id if id >= 4 => {
                let abbrev_idx = (id - 4) as usize;
                let abbrev = abbrevs
                    .get(abbrev_idx)
                    .ok_or_else(|| BitcodeError::ParseError(format!("unknown abbrev {}", id)))?;
                self.read_abbrev_record(&abbrev.ops.clone())?
            }
            _ => {
                // END_BLOCK / ENTER_SUBBLOCK / DEFINE_ABBREV — no data fields.
                Vec::new()
            }
        };
        Ok(fields)
    }

    /// Convenience: read the next abbrev_id and decode the record fields.
    ///
    /// Returns `(abbrev_id, fields)`.  Callers that need to dispatch on the
    /// abbrev_id (to call `enter_block`/`end_block`/`define_abbrev`) should
    /// use `read_bits(abbrev_len)` and `read_record_fields` directly instead.
    pub fn read_record(&mut self, abbrevs: &[Abbrev]) -> Result<(u64, Vec<u64>), BitcodeError> {
        let abbrev_id = self.read_bits(self.abbrev_len)?;
        let fields = self.read_record_fields(abbrev_id, abbrevs)?;
        Ok((abbrev_id, fields))
    }

    /// Decode one record's fields using the given abbreviation ops.
    fn read_abbrev_record(&mut self, ops: &[AbbrevOp]) -> Result<Vec<u64>, BitcodeError> {
        let mut fields = Vec::new();
        let mut op_iter = ops.iter();
        while let Some(op) = op_iter.next() {
            match op {
                AbbrevOp::Literal(v) => fields.push(*v),
                AbbrevOp::Fixed(w) => {
                    fields.push(self.read_bits(*w)?);
                }
                AbbrevOp::Vbr(w) => {
                    fields.push(self.read_vbr(*w)?);
                }
                AbbrevOp::Array => {
                    // Next op is the element encoding.
                    let elem_op = op_iter
                        .next()
                        .ok_or_else(|| BitcodeError::ParseError("Array without element op".into()))?
                        .clone();
                    let count = self.read_vbr(6)? as usize;
                    for _ in 0..count {
                        let v = self.read_abbrev_record(std::slice::from_ref(&elem_op))?;
                        fields.extend(v);
                    }
                }
                AbbrevOp::Char6 => {
                    let c = self.read_bits(6)?;
                    let byte: u8 = char6_to_ascii(c as u8)?;
                    fields.push(byte as u64);
                }
                AbbrevOp::Blob => {
                    let byte_len = self.read_vbr(6)? as usize;
                    self.align_32();
                    for _ in 0..byte_len {
                        let b = self.read_bits(8)?;
                        fields.push(b);
                    }
                    self.align_32();
                }
            }
        }
        Ok(fields)
    }
}

/// Decode a 6-bit Char6 code to ASCII.
fn char6_to_ascii(c: u8) -> Result<u8, BitcodeError> {
    match c {
        0..=25 => Ok(b'a' + c),
        26..=51 => Ok(b'A' + (c - 26)),
        52..=61 => Ok(b'0' + (c - 52)),
        62 => Ok(b'.'),
        63 => Ok(b'_'),
        _ => Err(BitcodeError::ParseError(format!(
            "invalid char6 code {}",
            c
        ))),
    }
}
