#![doc = include_str!("../README.md")]
#![no_std]
#![forbid(unsafe_code)]

use miniz_oxide::inflate::core::{decompress, inflate_flags, DecompressorOxide};
use miniz_oxide::inflate::TINFLStatus;

/// Required size of the caller-owned DEFLATE history ring.
pub const HISTORY_SIZE: usize = 32 * 1024;

const FIXED_HEADER_SIZE: usize = 10;
const TRAILER_SIZE: usize = 8;

/// Buffers borrowed by a [`Decoder`].
pub struct DecoderBuffers<'a> {
    /// The DEFLATE history ring. It must contain exactly [`HISTORY_SIZE`] bytes.
    pub history: &'a mut [u8],
}

/// Fixed fields from a gzip member header.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MemberHeader {
    pub modification_time: u32,
    pub extra_flags: u8,
    pub operating_system: u8,
}

/// Kind of progress made by one call to [`Decoder::decode`].
#[derive(Debug, Eq, PartialEq)]
pub enum DecodeStep<'a> {
    NeedInput {
        consumed: usize,
    },
    MemberStarted {
        consumed: usize,
        header: MemberHeader,
    },
    Output {
        consumed: usize,
        bytes: &'a [u8],
    },
    MemberFinished {
        consumed: usize,
    },
}

impl DecodeStep<'_> {
    pub const fn consumed(&self) -> usize {
        match self {
            Self::NeedInput { consumed }
            | Self::MemberStarted { consumed, .. }
            | Self::Output { consumed, .. }
            | Self::MemberFinished { consumed } => *consumed,
        }
    }
}

enum InternalStep {
    NeedInput,
    MemberStarted(MemberHeader),
    Output { start: usize, end: usize },
    MemberFinished,
}

/// A malformed stream or invalid decoder configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq, derive_more::Display)]
pub enum DecodeError {
    #[display("gzip history must contain {HISTORY_SIZE} bytes, got {actual}")]
    InvalidHistorySize { actual: usize },
    #[display("invalid gzip header")]
    InvalidHeader,
    #[display("unsupported gzip compression method {method}")]
    UnsupportedCompressionMethod { method: u8 },
    #[display("gzip header checksum mismatch")]
    InvalidHeaderChecksum,
    #[display("invalid DEFLATE stream")]
    InvalidDeflateStream,
    #[display("gzip data checksum mismatch: expected {expected:08x}, got {actual:08x}")]
    InvalidDataChecksum { expected: u32, actual: u32 },
    #[display("gzip data size mismatch: expected {expected}, got {actual}")]
    InvalidDataSize { expected: u32, actual: u32 },
    #[display("unexpected end of gzip stream")]
    UnexpectedEof,
    #[display("gzip decoder is poisoned")]
    DecoderPoisoned,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum State {
    Header,
    ExtraLength,
    Extra,
    Name,
    Comment,
    HeaderChecksum,
    StartDeflate,
    Deflate,
    Trailer,
}

/// Allocation-free incremental gzip decoder.
pub struct Decoder<'a> {
    history: &'a mut [u8],
    decompressor: DecompressorOxide,
    state: State,
    poisoned: bool,
    completed_members: u64,
    fixed: [u8; FIXED_HEADER_SIZE],
    fixed_len: usize,
    small: [u8; TRAILER_SIZE],
    small_len: usize,
    flags: u8,
    extra_remaining: usize,
    header_crc: u32,
    data_crc: u32,
    data_size: u32,
    output_position: usize,
    header: MemberHeader,
}

impl<'a> Decoder<'a> {
    pub fn new(buffers: DecoderBuffers<'a>) -> Result<Self, DecodeError> {
        if buffers.history.len() != HISTORY_SIZE {
            return Err(DecodeError::InvalidHistorySize {
                actual: buffers.history.len(),
            });
        }
        buffers.history.fill(0);
        Ok(Self {
            history: buffers.history,
            decompressor: DecompressorOxide::new(),
            state: State::Header,
            poisoned: false,
            completed_members: 0,
            fixed: [0; FIXED_HEADER_SIZE],
            fixed_len: 0,
            small: [0; TRAILER_SIZE],
            small_len: 0,
            flags: 0,
            extra_remaining: 0,
            header_crc: !0,
            data_crc: !0,
            data_size: 0,
            output_position: 0,
            header: MemberHeader {
                modification_time: 0,
                extra_flags: 0,
                operating_system: 0,
            },
        })
    }

    pub fn decode<'decoder>(
        &'decoder mut self,
        input: &[u8],
    ) -> Result<DecodeStep<'decoder>, DecodeError> {
        if self.poisoned {
            return Err(DecodeError::DecoderPoisoned);
        }
        let (consumed, step) = match self.decode_inner(input) {
            Ok(step) => step,
            Err(error) => {
                self.poisoned = true;
                return Err(error);
            }
        };
        Ok(match step {
            InternalStep::NeedInput => DecodeStep::NeedInput { consumed },
            InternalStep::MemberStarted(header) => DecodeStep::MemberStarted { consumed, header },
            InternalStep::Output { start, end } => DecodeStep::Output {
                consumed,
                bytes: &self.history[start..end],
            },
            InternalStep::MemberFinished => DecodeStep::MemberFinished { consumed },
        })
    }

    pub fn finish(&self) -> Result<(), DecodeError> {
        if self.poisoned {
            return Err(DecodeError::DecoderPoisoned);
        }
        if self.completed_members != 0
            && self.state == State::Header
            && self.fixed_len == 0
            && self.small_len == 0
        {
            Ok(())
        } else {
            Err(DecodeError::UnexpectedEof)
        }
    }

    pub fn reset(&mut self) {
        self.decompressor.init();
        self.history.fill(0);
        self.state = State::Header;
        self.poisoned = false;
        self.completed_members = 0;
        self.fixed_len = 0;
        self.small_len = 0;
        self.flags = 0;
        self.extra_remaining = 0;
        self.header_crc = !0;
        self.data_crc = !0;
        self.data_size = 0;
        self.output_position = 0;
    }

    fn decode_inner(&mut self, mut input: &[u8]) -> Result<(usize, InternalStep), DecodeError> {
        let original_length = input.len();
        loop {
            if let Some(step) = self.advance(&mut input)? {
                return Ok((original_length - input.len(), step));
            }
            if input.is_empty() {
                return Ok((original_length, InternalStep::NeedInput));
            }
        }
    }

    // None advances the state without exposing an event to the caller.
    fn advance(&mut self, input: &mut &[u8]) -> Result<Option<InternalStep>, DecodeError> {
        if !self.read_fields(input) {
            return Ok(Some(InternalStep::NeedInput));
        }
        match self.state {
            State::Header => self.read_member_header()?,
            State::ExtraLength => {
                self.extra_remaining = u16::from_le_bytes([self.small[0], self.small[1]]) as usize;
                self.small_len = 0;
                self.state = if self.extra_remaining == 0 {
                    self.next_optional_state()
                } else {
                    State::Extra
                };
            }
            State::Extra => {
                let length = self.extra_remaining.min(input.len());
                self.header_crc = crc32_update(self.header_crc, &input[..length]);
                *input = &input[length..];
                self.extra_remaining -= length;
                if self.extra_remaining != 0 {
                    return Ok(Some(InternalStep::NeedInput));
                }
                self.state = self.next_optional_state();
            }
            State::Name | State::Comment => {
                let remaining = *input;
                let Some(end) = remaining.iter().position(|byte| *byte == 0) else {
                    self.header_crc = crc32_update(self.header_crc, remaining);
                    *input = &[];
                    return Ok(Some(InternalStep::NeedInput));
                };
                let length = end + 1;
                self.header_crc = crc32_update(self.header_crc, &remaining[..length]);
                *input = &input[length..];
                self.state = self.next_optional_state();
            }
            State::HeaderChecksum => {
                let expected = u16::from_le_bytes([self.small[0], self.small[1]]);
                let actual = (!self.header_crc) as u16;
                if expected != actual {
                    return Err(DecodeError::InvalidHeaderChecksum);
                }
                self.small_len = 0;
                self.start_deflate();
                return Ok(Some(InternalStep::MemberStarted(self.header)));
            }
            State::StartDeflate => {
                self.start_deflate();
                return Ok(Some(InternalStep::MemberStarted(self.header)));
            }
            State::Deflate => return self.inflate(input),
            State::Trailer => {
                validate_trailer(&self.small, !self.data_crc, self.data_size)?;
                self.completed_members += 1;
                self.small_len = 0;
                self.state = State::Header;
                self.header_crc = !0;
                return Ok(Some(InternalStep::MemberFinished));
            }
        }
        Ok(None)
    }

    fn read_member_header(&mut self) -> Result<(), DecodeError> {
        if self.fixed[0..2] != [0x1f, 0x8b] || self.fixed[3] & 0xe0 != 0 {
            return Err(DecodeError::InvalidHeader);
        }
        if self.fixed[2] != 8 {
            return Err(DecodeError::UnsupportedCompressionMethod {
                method: self.fixed[2],
            });
        }
        self.flags = self.fixed[3];
        self.header = MemberHeader {
            modification_time: u32::from_le_bytes([
                self.fixed[4],
                self.fixed[5],
                self.fixed[6],
                self.fixed[7],
            ]),
            extra_flags: self.fixed[8],
            operating_system: self.fixed[9],
        };
        self.fixed_len = 0;
        self.small_len = 0;
        self.state = self.next_optional_state();

        Ok(())
    }

    fn inflate(&mut self, input: &mut &[u8]) -> Result<Option<InternalStep>, DecodeError> {
        let start = self.output_position;
        let (status, consumed, written) = decompress(
            &mut self.decompressor,
            input,
            self.history,
            start,
            inflate_flags::TINFL_FLAG_HAS_MORE_INPUT,
        );
        *input = &input[consumed..];
        let end = start + written;
        // miniz never wraps within a single call, so `history[start..end]`
        // is contiguous and in bounds. `output_position` below preserves
        // `start < len`, which also guarantees at least one free byte.
        debug_assert!(end <= self.history.len());
        self.data_crc = crc32_update(self.data_crc, &self.history[start..end]);
        self.data_size = self.data_size.wrapping_add(written as u32);
        self.output_position = if end == self.history.len() { 0 } else { end };
        match status {
            TINFLStatus::Done => self.state = State::Trailer,
            TINFLStatus::HasMoreOutput | TINFLStatus::NeedsMoreInput => {}
            _ => return Err(DecodeError::InvalidDeflateStream),
        }
        if written != 0 {
            return Ok(Some(InternalStep::Output { start, end }));
        }
        if needs_input_without_output(status)? {
            return Ok(Some(InternalStep::NeedInput));
        }

        Ok(None)
    }

    fn read_fields(&mut self, input: &mut &[u8]) -> bool {
        let (buffer, filled, checksummed): (&mut [u8], &mut usize, bool) = match self.state {
            State::Header => (&mut self.fixed, &mut self.fixed_len, true),
            State::ExtraLength => (&mut self.small[..2], &mut self.small_len, true),
            State::HeaderChecksum => (&mut self.small[..2], &mut self.small_len, false),
            State::Trailer => (&mut self.small, &mut self.small_len, false),
            _ => return true,
        };
        let copied = copy_into(buffer, filled, input);
        if checksummed {
            self.header_crc = crc32_update(self.header_crc, &input[..copied]);
        }
        *input = &input[copied..];
        *filled == buffer.len()
    }

    fn next_optional_state(&mut self) -> State {
        // Consume optional fields in wire order, clearing each flag as selected.
        for (flag, state) in [
            (0x04, State::ExtraLength),
            (0x08, State::Name),
            (0x10, State::Comment),
            (0x02, State::HeaderChecksum),
        ] {
            if self.flags & flag != 0 {
                self.flags &= !flag;
                return state;
            }
        }
        State::StartDeflate
    }

    fn start_deflate(&mut self) {
        self.decompressor.init();
        // Load-bearing for cross-member isolation; do not remove as an optimization.
        //
        // `history` doubles as miniz's wrapping output window, so a malformed member
        // whose DEFLATE stream back-references before its own output resolves that
        // distance against whatever the ring still holds. miniz does not reject
        // out-of-range distances in wrapping mode — it masks them — so without this
        // zeroing the previous member's plaintext is emitted as this member's output.
        // Covered by `tests/invariants.rs`.
        self.history.fill(0);
        self.data_crc = !0;
        self.data_size = 0;
        self.output_position = 0;
        self.state = State::Deflate;
    }
}

fn needs_input_without_output(status: TINFLStatus) -> Result<bool, DecodeError> {
    match status {
        TINFLStatus::Done => Ok(false),
        TINFLStatus::NeedsMoreInput => Ok(true),
        // Unreachable while `output_position < history.len()`: a full output
        // buffer always has non-empty output, handled before this function.
        TINFLStatus::HasMoreOutput => Err(DecodeError::InvalidDeflateStream),
        _ => unreachable!(),
    }
}

fn validate_trailer(
    trailer: &[u8; TRAILER_SIZE],
    actual_crc: u32,
    actual_size: u32,
) -> Result<(), DecodeError> {
    let expected_crc = u32::from_le_bytes([trailer[0], trailer[1], trailer[2], trailer[3]]);
    if expected_crc != actual_crc {
        return Err(DecodeError::InvalidDataChecksum {
            expected: expected_crc,
            actual: actual_crc,
        });
    }
    let expected_size = u32::from_le_bytes([trailer[4], trailer[5], trailer[6], trailer[7]]);
    if expected_size != actual_size {
        return Err(DecodeError::InvalidDataSize {
            expected: expected_size,
            actual: actual_size,
        });
    }
    Ok(())
}

fn copy_into(destination: &mut [u8], filled: &mut usize, input: &[u8]) -> usize {
    let length = input.len().min(destination.len() - *filled);
    destination[*filled..*filled + length].copy_from_slice(&input[..length]);
    *filled += length;
    length
}

fn crc32_update(crc: u32, bytes: &[u8]) -> u32 {
    // Preserve the decoder's unfinalized CRC convention across fragments.
    let mut hash = crc32fast::Hasher::new_with_initial(!crc);
    hash.update(bytes);
    !hash.finalize()
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::string::ToString;

    use super::*;

    fn new_decoder(history: &mut [u8; HISTORY_SIZE]) -> Decoder<'_> {
        Decoder::new(DecoderBuffers { history }).unwrap()
    }

    #[test]
    fn formats_decode_errors() {
        assert_eq!(
            DecodeError::InvalidDataChecksum {
                expected: 0x1234,
                actual: 0xabcd,
            }
            .to_string(),
            "gzip data checksum mismatch: expected 00001234, got 0000abcd"
        );
    }

    #[test]
    fn finish_requires_a_completed_member_at_a_clean_header_boundary() {
        let mut history = [0; HISTORY_SIZE];
        let mut decoder = new_decoder(&mut history);

        assert_eq!(decoder.finish(), Err(DecodeError::UnexpectedEof));

        decoder.completed_members = 1;
        assert_eq!(decoder.finish(), Ok(()));

        decoder.state = State::Deflate;
        assert_eq!(decoder.finish(), Err(DecodeError::UnexpectedEof));
        decoder.state = State::Header;

        decoder.fixed_len = 1;
        assert_eq!(decoder.finish(), Err(DecodeError::UnexpectedEof));
        decoder.fixed_len = 0;

        decoder.small_len = 1;
        assert_eq!(decoder.finish(), Err(DecodeError::UnexpectedEof));
    }

    #[test]
    fn reset_restores_every_stream_state_field() {
        let mut history = [0; HISTORY_SIZE];
        let mut decoder = new_decoder(&mut history);
        decoder.history.fill(0xff);
        decoder.state = State::Trailer;
        decoder.poisoned = true;
        decoder.completed_members = 1;
        decoder.fixed_len = 1;
        decoder.small_len = 1;
        decoder.flags = 0xff;
        decoder.extra_remaining = 1;
        decoder.header_crc = 0;
        decoder.data_crc = 0;
        decoder.data_size = 1;
        decoder.output_position = 1;

        decoder.reset();

        assert!(decoder.history.iter().all(|byte| *byte == 0));
        assert_eq!(decoder.state, State::Header);
        assert!(!decoder.poisoned);
        assert_eq!(decoder.completed_members, 0);
        assert_eq!(decoder.fixed_len, 0);
        assert_eq!(decoder.small_len, 0);
        assert_eq!(decoder.flags, 0);
        assert_eq!(decoder.extra_remaining, 0);
        assert_eq!(decoder.header_crc, !0);
        assert_eq!(decoder.data_crc, !0);
        assert_eq!(decoder.data_size, 0);
        assert_eq!(decoder.output_position, 0);
    }

    #[test]
    fn rejects_bad_magic_and_reserved_flags_independently() {
        for header in [
            [0, 0, 8, 0, 0, 0, 0, 0, 0, 0],
            [0x1f, 0x8b, 8, 0x20, 0, 0, 0, 0, 0, 0],
        ] {
            let mut history = [0; HISTORY_SIZE];
            assert_eq!(
                new_decoder(&mut history).decode(&header),
                Err(DecodeError::InvalidHeader)
            );
        }
    }

    #[test]
    fn recognizes_the_extra_field_flag_exactly() {
        let mut history = [0; HISTORY_SIZE];
        let mut decoder = new_decoder(&mut history);

        decoder.flags = 0;
        assert_eq!(decoder.next_optional_state(), State::StartDeflate);
        decoder.flags = 0x04;
        assert_eq!(decoder.next_optional_state(), State::ExtraLength);
    }

    #[test]
    fn starts_deflate_from_clean_state() {
        let mut history = [0; HISTORY_SIZE];
        let mut decoder = new_decoder(&mut history);
        decoder.history.fill(0xff);
        decoder.data_crc = 0;
        decoder.data_size = 1;
        decoder.output_position = 1;

        decoder.start_deflate();

        assert!(decoder.history.iter().all(|byte| *byte == 0));
        assert_eq!(decoder.data_crc, !0);
        assert_eq!(decoder.data_size, 0);
        assert_eq!(decoder.output_position, 0);
        assert_eq!(decoder.state, State::Deflate);
    }

    #[test]
    fn classifies_statuses_without_output() {
        assert_eq!(needs_input_without_output(TINFLStatus::Done), Ok(false));
        assert_eq!(
            needs_input_without_output(TINFLStatus::NeedsMoreInput),
            Ok(true)
        );
        assert_eq!(
            needs_input_without_output(TINFLStatus::HasMoreOutput),
            Err(DecodeError::InvalidDeflateStream)
        );
    }

    #[test]
    fn validates_trailer_length_checksum_and_size() {
        let mut history = [0; HISTORY_SIZE];
        let mut decoder = new_decoder(&mut history);
        decoder.state = State::Trailer;
        let mut input = &[0; TRAILER_SIZE - 1][..];
        assert!(!decoder.read_fields(&mut input));
        let mut input = &[0][..];
        assert!(decoder.read_fields(&mut input));

        let mut trailer = [0; TRAILER_SIZE];
        trailer[..4].copy_from_slice(&0x1234_u32.to_le_bytes());
        trailer[4..].copy_from_slice(&56_u32.to_le_bytes());
        assert_eq!(validate_trailer(&trailer, 0x1234, 56), Ok(()));
        assert_eq!(
            validate_trailer(&trailer, 0xabcd, 56),
            Err(DecodeError::InvalidDataChecksum {
                expected: 0x1234,
                actual: 0xabcd,
            })
        );
        assert_eq!(
            validate_trailer(&trailer, 0x1234, 78),
            Err(DecodeError::InvalidDataSize {
                expected: 56,
                actual: 78,
            })
        );
    }

    #[test]
    fn copies_into_the_unfilled_destination_suffix() {
        let mut destination = [1, 0, 0];
        let mut filled = 1;

        assert_eq!(copy_into(&mut destination, &mut filled, &[2, 3, 4]), 2);
        assert_eq!(destination, [1, 2, 3]);
        assert_eq!(filled, 3);
    }
}
