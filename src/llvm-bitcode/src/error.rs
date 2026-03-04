//! Bitcode error types.

/// Error type for bitcode reading and writing.
#[derive(Debug)]
pub enum BitcodeError {
    /// Magic bytes did not match the LRIR format header.
    InvalidMagic,
    /// Input ended before a field could be fully read.
    TruncatedInput,
    /// Unexpected end-of-file inside a structured record.
    UnexpectedEof,
    /// A record type tag was not recognised.
    UnsupportedRecord(u32),
    /// A type tag was not a recognised `TypeTag` value.
    InvalidType,
    /// A general parse error with a description.
    ParseError(String),
}

impl std::fmt::Display for BitcodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BitcodeError::InvalidMagic        => write!(f, "invalid magic bytes (not LRIR format)"),
            BitcodeError::TruncatedInput      => write!(f, "input is truncated"),
            BitcodeError::UnexpectedEof       => write!(f, "unexpected end of file"),
            BitcodeError::UnsupportedRecord(t) => write!(f, "unsupported record type: {}", t),
            BitcodeError::InvalidType         => write!(f, "invalid type tag"),
            BitcodeError::ParseError(msg)     => write!(f, "parse error: {}", msg),
        }
    }
}

impl std::error::Error for BitcodeError {}
