#![doc = include_str!("../README.md")]
#![no_std]
#![forbid(unsafe_code)]

use core::fmt;

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
    NeedInput {
        consumed: usize,
    },
    MemberStarted {
        consumed: usize,
        header: MemberHeader,
    },
    Output {
        consumed: usize,
        start: usize,
        end: usize,
    },
    MemberFinished {
        consumed: usize,
    },
}

/// A malformed stream or invalid decoder configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DecodeError {
    InvalidHistorySize { actual: usize },
    InvalidHeader,
    UnsupportedCompressionMethod { method: u8 },
    InvalidHeaderChecksum,
    InvalidDeflateStream,
    InvalidDataChecksum { expected: u32, actual: u32 },
    InvalidDataSize { expected: u32, actual: u32 },
    UnexpectedEof,
    DecoderPoisoned,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidHistorySize { actual } => write!(
                formatter,
                "gzip history must contain {HISTORY_SIZE} bytes, got {actual}"
            ),
            Self::InvalidHeader => formatter.write_str("invalid gzip header"),
            Self::UnsupportedCompressionMethod { method } => {
                write!(formatter, "unsupported gzip compression method {method}")
            }
            Self::InvalidHeaderChecksum => formatter.write_str("gzip header checksum mismatch"),
            Self::InvalidDeflateStream => formatter.write_str("invalid DEFLATE stream"),
            Self::InvalidDataChecksum { expected, actual } => write!(
                formatter,
                "gzip data checksum mismatch: expected {expected:08x}, got {actual:08x}"
            ),
            Self::InvalidDataSize { expected, actual } => write!(
                formatter,
                "gzip data size mismatch: expected {expected}, got {actual}"
            ),
            Self::UnexpectedEof => formatter.write_str("unexpected end of gzip stream"),
            Self::DecoderPoisoned => formatter.write_str("gzip decoder is poisoned"),
        }
    }
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
        let step = match self.decode_inner(input) {
            Ok(step) => step,
            Err(error) => {
                self.poisoned = true;
                return Err(error);
            }
        };
        Ok(match step {
            InternalStep::NeedInput { consumed } => DecodeStep::NeedInput { consumed },
            InternalStep::MemberStarted { consumed, header } => {
                DecodeStep::MemberStarted { consumed, header }
            }
            InternalStep::Output {
                consumed,
                start,
                end,
            } => DecodeStep::Output {
                consumed,
                bytes: &self.history[start..end],
            },
            InternalStep::MemberFinished { consumed } => DecodeStep::MemberFinished { consumed },
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

    fn decode_inner(&mut self, input: &[u8]) -> Result<InternalStep, DecodeError> {
        let mut position = 0;
        loop {
            match self.state {
                State::Header => {
                    let copied =
                        copy_into(&mut self.fixed, &mut self.fixed_len, &input[position..]);
                    self.header_crc =
                        crc32_update(self.header_crc, &input[position..position + copied]);
                    position += copied;
                    if self.fixed_len != FIXED_HEADER_SIZE {
                        return Ok(InternalStep::NeedInput { consumed: position });
                    }
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
                }
                State::ExtraLength => {
                    let copied = copy_into(
                        &mut self.small[..2],
                        &mut self.small_len,
                        &input[position..],
                    );
                    self.header_crc =
                        crc32_update(self.header_crc, &input[position..position + copied]);
                    position += copied;
                    if self.small_len != 2 {
                        return Ok(InternalStep::NeedInput { consumed: position });
                    }
                    self.extra_remaining =
                        u16::from_le_bytes([self.small[0], self.small[1]]) as usize;
                    self.small_len = 0;
                    self.state = if self.extra_remaining == 0 {
                        self.after_extra()
                    } else {
                        State::Extra
                    };
                }
                State::Extra => {
                    let length = self.extra_remaining.min(input.len() - position);
                    self.header_crc =
                        crc32_update(self.header_crc, &input[position..position + length]);
                    position += length;
                    self.extra_remaining -= length;
                    if self.extra_remaining != 0 {
                        return Ok(InternalStep::NeedInput { consumed: position });
                    }
                    self.state = self.after_extra();
                }
                State::Name | State::Comment => {
                    let remaining = &input[position..];
                    let Some(end) = remaining.iter().position(|byte| *byte == 0) else {
                        self.header_crc = crc32_update(self.header_crc, remaining);
                        return Ok(InternalStep::NeedInput {
                            consumed: input.len(),
                        });
                    };
                    let length = end + 1;
                    self.header_crc = crc32_update(self.header_crc, &remaining[..length]);
                    position += length;
                    self.state = if self.state == State::Name {
                        self.after_name()
                    } else {
                        self.after_comment()
                    };
                }
                State::HeaderChecksum => {
                    let copied = copy_into(
                        &mut self.small[..2],
                        &mut self.small_len,
                        &input[position..],
                    );
                    position += copied;
                    if self.small_len != 2 {
                        return Ok(InternalStep::NeedInput { consumed: position });
                    }
                    let expected = u16::from_le_bytes([self.small[0], self.small[1]]);
                    let actual = (!self.header_crc) as u16;
                    if expected != actual {
                        return Err(DecodeError::InvalidHeaderChecksum);
                    }
                    self.small_len = 0;
                    self.start_deflate();
                    return Ok(InternalStep::MemberStarted {
                        consumed: position,
                        header: self.header,
                    });
                }
                State::StartDeflate => {
                    self.start_deflate();
                    return Ok(InternalStep::MemberStarted {
                        consumed: position,
                        header: self.header,
                    });
                }
                State::Deflate => {
                    let start = self.output_position;
                    let (status, consumed, written) = decompress(
                        &mut self.decompressor,
                        &input[position..],
                        self.history,
                        start,
                        inflate_flags::TINFL_FLAG_HAS_MORE_INPUT,
                    );
                    position += consumed;
                    let end = start + written;
                    self.data_crc = crc32_update(self.data_crc, &self.history[start..end]);
                    self.data_size = self.data_size.wrapping_add(written as u32);
                    self.output_position = if end == self.history.len() { 0 } else { end };
                    match status {
                        TINFLStatus::Done => self.state = State::Trailer,
                        TINFLStatus::HasMoreOutput | TINFLStatus::NeedsMoreInput => {}
                        _ => return Err(DecodeError::InvalidDeflateStream),
                    }
                    if written != 0 {
                        return Ok(InternalStep::Output {
                            consumed: position,
                            start,
                            end,
                        });
                    }
                    match status {
                        TINFLStatus::Done => {}
                        TINFLStatus::NeedsMoreInput => {
                            return Ok(InternalStep::NeedInput { consumed: position });
                        }
                        TINFLStatus::HasMoreOutput => {
                            return Err(DecodeError::InvalidDeflateStream);
                        }
                        _ => unreachable!(),
                    }
                }
                State::Trailer => {
                    let copied =
                        copy_into(&mut self.small, &mut self.small_len, &input[position..]);
                    position += copied;
                    if self.small_len != TRAILER_SIZE {
                        return Ok(InternalStep::NeedInput { consumed: position });
                    }
                    let expected_crc = u32::from_le_bytes([
                        self.small[0],
                        self.small[1],
                        self.small[2],
                        self.small[3],
                    ]);
                    let actual_crc = !self.data_crc;
                    if expected_crc != actual_crc {
                        return Err(DecodeError::InvalidDataChecksum {
                            expected: expected_crc,
                            actual: actual_crc,
                        });
                    }
                    let expected_size = u32::from_le_bytes([
                        self.small[4],
                        self.small[5],
                        self.small[6],
                        self.small[7],
                    ]);
                    if expected_size != self.data_size {
                        return Err(DecodeError::InvalidDataSize {
                            expected: expected_size,
                            actual: self.data_size,
                        });
                    }
                    self.completed_members += 1;
                    self.small_len = 0;
                    self.state = State::Header;
                    self.header_crc = !0;
                    return Ok(InternalStep::MemberFinished { consumed: position });
                }
            }

            if position == input.len() {
                return Ok(InternalStep::NeedInput { consumed: position });
            }
        }
    }

    fn next_optional_state(&self) -> State {
        if self.flags & 0x04 != 0 {
            State::ExtraLength
        } else {
            self.after_extra()
        }
    }

    fn after_extra(&self) -> State {
        if self.flags & 0x08 != 0 {
            State::Name
        } else {
            self.after_name()
        }
    }

    fn after_name(&self) -> State {
        if self.flags & 0x10 != 0 {
            State::Comment
        } else {
            self.after_comment()
        }
    }

    fn after_comment(&self) -> State {
        if self.flags & 0x02 != 0 {
            State::HeaderChecksum
        } else {
            State::StartDeflate
        }
    }

    fn start_deflate(&mut self) {
        self.decompressor.init();
        self.history.fill(0);
        self.data_crc = !0;
        self.data_size = 0;
        self.output_position = 0;
        self.state = State::Deflate;
    }
}

fn copy_into(destination: &mut [u8], filled: &mut usize, input: &[u8]) -> usize {
    let length = input.len().min(destination.len() - *filled);
    destination[*filled..*filled + length].copy_from_slice(&input[..length]);
    *filled += length;
    length
}

fn crc32_update(mut crc: u32, bytes: &[u8]) -> u32 {
    for byte in bytes {
        crc = CRC32_TABLE[((crc ^ u32::from(*byte)) & 0xff) as usize] ^ (crc >> 8);
    }
    crc
}

const fn crc32_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut index = 0;
    while index < table.len() {
        let mut value = index as u32;
        let mut bit = 0;
        while bit < 8 {
            value = if value & 1 == 0 {
                value >> 1
            } else {
                0xedb8_8320 ^ (value >> 1)
            };
            bit += 1;
        }
        table[index] = value;
        index += 1;
    }
    table
}

const CRC32_TABLE: [u32; 256] = crc32_table();
