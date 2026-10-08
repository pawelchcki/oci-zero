#[derive(Clone, Copy, Debug, Eq, PartialEq, derive_more::Display)]
#[non_exhaustive]
pub enum DecodeError {
    #[display("decoder is poisoned")]
    DecoderPoisoned,
    #[display("invalid zstd frame magic")]
    InvalidMagic,
    #[display("invalid zstd frame header")]
    InvalidFrameHeader,
    #[display("zstd dictionary {id} is not supported")]
    UnsupportedDictionary { id: u32 },
    #[display("zstd window does not fit this platform")]
    WindowTooLarge,
    #[display("history buffer is too small: need {required} bytes, have {provided}")]
    HistoryTooSmall { required: usize, provided: usize },
    #[display("block scratch is too small: need {required} bytes, have {provided}")]
    BlockScratchTooSmall { required: usize, provided: usize },
    #[display("literal scratch is too small: need {required} bytes, have {provided}")]
    LiteralScratchTooSmall { required: usize, provided: usize },
    #[display("invalid zstd block")]
    InvalidBlock,
    #[display("invalid zstd entropy table")]
    InvalidEntropyTable,
    #[display("invalid zstd bitstream")]
    InvalidBitstream,
    #[display("invalid zstd match offset")]
    InvalidOffset,
    #[display("zstd checksum mismatch: expected {expected:08x}, got {actual:08x}")]
    ChecksumMismatch { expected: u32, actual: u32 },
    #[display("zstd content-size mismatch: expected {expected}, got {actual}")]
    ContentSizeMismatch { expected: u64, actual: u64 },
    #[display("unexpected end of zstd stream")]
    UnexpectedEof,
    #[display("zstd size arithmetic overflow")]
    ArithmeticOverflow,
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
