use core::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum DecodeError {
    DecoderPoisoned,
    InvalidMagic,
    InvalidFrameHeader,
    UnsupportedDictionary { id: u32 },
    WindowTooLarge,
    HistoryTooSmall { required: usize, provided: usize },
    BlockScratchTooSmall { required: usize, provided: usize },
    LiteralScratchTooSmall { required: usize, provided: usize },
    InvalidBlock,
    InvalidEntropyTable,
    InvalidBitstream,
    InvalidOffset,
    ChecksumMismatch { expected: u32, actual: u32 },
    ContentSizeMismatch { expected: u64, actual: u64 },
    UnexpectedEof,
    ArithmeticOverflow,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DecoderPoisoned => formatter.write_str("decoder is poisoned"),
            Self::InvalidMagic => formatter.write_str("invalid zstd frame magic"),
            Self::InvalidFrameHeader => formatter.write_str("invalid zstd frame header"),
            Self::UnsupportedDictionary { id } => {
                write!(formatter, "zstd dictionary {id} is not supported")
            }
            Self::WindowTooLarge => formatter.write_str("zstd window does not fit this platform"),
            Self::HistoryTooSmall { required, provided } => write!(
                formatter,
                "history buffer is too small: need {required} bytes, have {provided}"
            ),
            Self::BlockScratchTooSmall { required, provided } => write!(
                formatter,
                "block scratch is too small: need {required} bytes, have {provided}"
            ),
            Self::LiteralScratchTooSmall { required, provided } => write!(
                formatter,
                "literal scratch is too small: need {required} bytes, have {provided}"
            ),
            Self::InvalidBlock => formatter.write_str("invalid zstd block"),
            Self::InvalidEntropyTable => formatter.write_str("invalid zstd entropy table"),
            Self::InvalidBitstream => formatter.write_str("invalid zstd bitstream"),
            Self::InvalidOffset => formatter.write_str("invalid zstd match offset"),
            Self::ChecksumMismatch { expected, actual } => write!(
                formatter,
                "zstd checksum mismatch: expected {expected:08x}, got {actual:08x}"
            ),
            Self::ContentSizeMismatch { expected, actual } => write!(
                formatter,
                "zstd content-size mismatch: expected {expected}, got {actual}"
            ),
            Self::UnexpectedEof => formatter.write_str("unexpected end of zstd stream"),
            Self::ArithmeticOverflow => formatter.write_str("zstd size arithmetic overflow"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::string::ToString;

    use super::DecodeError;

    #[test]
    fn formats_every_decode_error() {
        let cases = [
            (DecodeError::DecoderPoisoned, "decoder is poisoned"),
            (DecodeError::InvalidMagic, "invalid zstd frame magic"),
            (DecodeError::InvalidFrameHeader, "invalid zstd frame header"),
            (
                DecodeError::UnsupportedDictionary { id: 42 },
                "zstd dictionary 42 is not supported",
            ),
            (
                DecodeError::WindowTooLarge,
                "zstd window does not fit this platform",
            ),
            (
                DecodeError::HistoryTooSmall {
                    required: 8,
                    provided: 3,
                },
                "history buffer is too small: need 8 bytes, have 3",
            ),
            (
                DecodeError::BlockScratchTooSmall {
                    required: 8,
                    provided: 3,
                },
                "block scratch is too small: need 8 bytes, have 3",
            ),
            (
                DecodeError::LiteralScratchTooSmall {
                    required: 8,
                    provided: 3,
                },
                "literal scratch is too small: need 8 bytes, have 3",
            ),
            (DecodeError::InvalidBlock, "invalid zstd block"),
            (
                DecodeError::InvalidEntropyTable,
                "invalid zstd entropy table",
            ),
            (DecodeError::InvalidBitstream, "invalid zstd bitstream"),
            (DecodeError::InvalidOffset, "invalid zstd match offset"),
            (
                DecodeError::ChecksumMismatch {
                    expected: 0x1234,
                    actual: 0xabcd,
                },
                "zstd checksum mismatch: expected 00001234, got 0000abcd",
            ),
            (
                DecodeError::ContentSizeMismatch {
                    expected: 12,
                    actual: 34,
                },
                "zstd content-size mismatch: expected 12, got 34",
            ),
            (DecodeError::UnexpectedEof, "unexpected end of zstd stream"),
            (
                DecodeError::ArithmeticOverflow,
                "zstd size arithmetic overflow",
            ),
        ];

        for (error, expected) in cases {
            assert_eq!(error.to_string(), expected);
        }
    }
}
