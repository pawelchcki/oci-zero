#![doc = include_str!("../README.md")]
#![no_std]
#![forbid(unsafe_code)]

mod bitstream;
mod error;
mod fse;
mod huffman;
mod xxhash;

use bitstream::BackwardBits;
pub use error::DecodeError;
use fse::Table as FseTable;
use huffman::Table as HuffmanTable;
use xxhash::XxHash64;

pub const MAX_BLOCK_SIZE: usize = 128 * 1024;
pub const MAX_FRAME_HEADER_SIZE: usize = 18;

const ZSTD_MAGIC: u32 = 0xfd2f_b528;
const SKIPPABLE_MAGIC_MIN: u32 = 0x184d_2a50;
const SKIPPABLE_MAGIC_MAX: u32 = 0x184d_2a5f;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameHeader {
    pub window_size: u64,
    pub content_size: Option<u64>,
    pub dictionary_id: u32,
    pub has_checksum: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StreamHeader {
    Zstandard(FrameHeader),
    Skippable { magic: u32, size: u32 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameKind {
    Zstandard,
    Skippable { magic: u32, size: u32 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HeaderStatus {
    NeedMore { minimum: usize },
    Complete { header: StreamHeader, size: usize },
}

#[derive(Debug, Eq, PartialEq)]
pub enum DecodeStep<'a> {
    NeedInput {
        consumed: usize,
    },
    FrameStarted {
        consumed: usize,
        header: StreamHeader,
    },
    Output {
        consumed: usize,
        bytes: &'a [u8],
    },
    FrameFinished {
        consumed: usize,
        kind: FrameKind,
    },
}

impl DecodeStep<'_> {
    pub const fn consumed(&self) -> usize {
        match self {
            Self::NeedInput { consumed }
            | Self::FrameStarted { consumed, .. }
            | Self::Output { consumed, .. }
            | Self::FrameFinished { consumed, .. } => *consumed,
        }
    }
}

enum InternalStep {
    NeedInput {
        consumed: usize,
    },
    FrameStarted {
        consumed: usize,
        header: StreamHeader,
    },
    Output {
        consumed: usize,
        start: usize,
        length: usize,
    },
    FrameFinished {
        consumed: usize,
        kind: FrameKind,
    },
}

pub struct DecoderBuffers<'a> {
    pub history: &'a mut [u8],
    pub block: &'a mut [u8],
    pub literals: &'a mut [u8],
}

#[derive(Clone, Copy)]
enum State {
    Header,
    Skippable {
        magic: u32,
        size: u32,
        remaining: usize,
    },
    BlockHeader,
    BlockPayload {
        header: BlockHeader,
        filled: usize,
    },
    Checksum {
        filled: usize,
    },
    FrameDone(FrameKind),
}

#[derive(Clone, Copy)]
struct BlockHeader {
    last: bool,
    kind: BlockKind,
    size: usize,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum BlockKind {
    Raw,
    Rle,
    Compressed,
}

pub struct Decoder<'a> {
    history: &'a mut [u8],
    block: &'a mut [u8],
    literals: &'a mut [u8],
    state: State,
    poisoned: bool,
    completed_frames: u64,
    header_buffer: [u8; MAX_FRAME_HEADER_SIZE],
    header_len: usize,
    block_header_buffer: [u8; 3],
    block_header_len: usize,
    checksum_buffer: [u8; 4],
    current_header: Option<FrameHeader>,
    block_limit: usize,
    history_position: usize,
    frame_output: u64,
    pending_start: usize,
    pending_len: usize,
    offsets: [u32; 3],
    checksum: XxHash64,
    huffman: HuffmanTable,
    literal_lengths: FseTable,
    offsets_table: FseTable,
    match_lengths: FseTable,
}

impl<'a> Decoder<'a> {
    pub fn new(buffers: DecoderBuffers<'a>) -> Self {
        Self {
            history: buffers.history,
            block: buffers.block,
            literals: buffers.literals,
            state: State::Header,
            poisoned: false,
            completed_frames: 0,
            header_buffer: [0; MAX_FRAME_HEADER_SIZE],
            header_len: 0,
            block_header_buffer: [0; 3],
            block_header_len: 0,
            checksum_buffer: [0; 4],
            current_header: None,
            block_limit: 0,
            history_position: 0,
            frame_output: 0,
            pending_start: 0,
            pending_len: 0,
            offsets: [1, 4, 8],
            checksum: XxHash64::new(),
            huffman: HuffmanTable::new(),
            literal_lengths: FseTable::new(),
            offsets_table: FseTable::new(),
            match_lengths: FseTable::new(),
        }
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
            InternalStep::FrameStarted { consumed, header } => {
                DecodeStep::FrameStarted { consumed, header }
            }
            InternalStep::Output {
                consumed,
                start,
                length,
            } => DecodeStep::Output {
                consumed,
                bytes: &self.history[start..start + length],
            },
            InternalStep::FrameFinished { consumed, kind } => {
                DecodeStep::FrameFinished { consumed, kind }
            }
        })
    }

    pub fn finish(&self) -> Result<(), DecodeError> {
        if self.poisoned {
            return Err(DecodeError::DecoderPoisoned);
        }
        if self.pending_len != 0 || self.header_len != 0 || self.completed_frames == 0 {
            return Err(DecodeError::UnexpectedEof);
        }
        match self.state {
            State::Header | State::FrameDone(_) => Ok(()),
            _ => Err(DecodeError::UnexpectedEof),
        }
    }

    pub fn reset(&mut self) {
        self.state = State::Header;
        self.poisoned = false;
        self.completed_frames = 0;
        self.header_len = 0;
        self.block_header_len = 0;
        self.current_header = None;
        self.frame_output = 0;
        self.pending_len = 0;
        self.offsets = [1, 4, 8];
        self.checksum = XxHash64::new();
        self.huffman = HuffmanTable::new();
        self.literal_lengths = FseTable::new();
        self.offsets_table = FseTable::new();
        self.match_lengths = FseTable::new();
    }

    fn decode_inner(&mut self, input: &[u8]) -> Result<InternalStep, DecodeError> {
        if self.pending_len != 0 {
            let start = self.pending_start;
            let length = core::cmp::min(self.pending_len, self.history.len() - start);
            self.pending_start = (start + length) % self.history.len();
            self.pending_len -= length;
            return Ok(InternalStep::Output {
                consumed: 0,
                start,
                length,
            });
        }

        let mut consumed = 0usize;
        loop {
            match self.state {
                State::Header => match inspect_frame(&self.header_buffer[..self.header_len])? {
                    HeaderStatus::NeedMore { minimum } => {
                        if consumed == input.len() {
                            return Ok(InternalStep::NeedInput { consumed });
                        }
                        let amount =
                            core::cmp::min(minimum - self.header_len, input.len() - consumed);
                        self.header_buffer[self.header_len..self.header_len + amount]
                            .copy_from_slice(&input[consumed..consumed + amount]);
                        self.header_len += amount;
                        consumed += amount;
                    }
                    HeaderStatus::Complete { header, .. } => {
                        self.header_len = 0;
                        match header {
                            StreamHeader::Zstandard(frame) => {
                                self.start_frame(frame)?;
                                self.state = State::BlockHeader;
                            }
                            StreamHeader::Skippable { magic, size } => {
                                self.state = State::Skippable {
                                    magic,
                                    size,
                                    remaining: size as usize,
                                };
                            }
                        }
                        return Ok(InternalStep::FrameStarted { consumed, header });
                    }
                },
                State::Skippable {
                    magic,
                    size,
                    remaining,
                } => {
                    let amount = core::cmp::min(remaining, input.len() - consumed);
                    consumed += amount;
                    let remaining = remaining - amount;
                    if remaining == 0 {
                        self.state = State::FrameDone(FrameKind::Skippable { magic, size });
                    } else {
                        self.state = State::Skippable {
                            magic,
                            size,
                            remaining,
                        };
                        return Ok(InternalStep::NeedInput { consumed });
                    }
                }
                State::BlockHeader => {
                    let amount = core::cmp::min(3 - self.block_header_len, input.len() - consumed);
                    self.block_header_buffer[self.block_header_len..self.block_header_len + amount]
                        .copy_from_slice(&input[consumed..consumed + amount]);
                    self.block_header_len += amount;
                    consumed += amount;
                    if self.block_header_len != 3 {
                        return Ok(InternalStep::NeedInput { consumed });
                    }
                    self.block_header_len = 0;
                    let header = parse_block_header(self.block_header_buffer, self.block_limit)?;
                    let payload_size = if header.kind == BlockKind::Rle {
                        1
                    } else {
                        header.size
                    };
                    if payload_size > self.block.len() {
                        return Err(DecodeError::BlockScratchTooSmall {
                            required: payload_size,
                            provided: self.block.len(),
                        });
                    }
                    self.state = State::BlockPayload { header, filled: 0 };
                }
                State::BlockPayload { header, filled } => {
                    let payload_size = if header.kind == BlockKind::Rle {
                        1
                    } else {
                        header.size
                    };
                    let amount = core::cmp::min(payload_size - filled, input.len() - consumed);
                    self.block[filled..filled + amount]
                        .copy_from_slice(&input[consumed..consumed + amount]);
                    consumed += amount;
                    let filled = filled + amount;
                    if filled != payload_size {
                        self.state = State::BlockPayload { header, filled };
                        return Ok(InternalStep::NeedInput { consumed });
                    }
                    self.process_block(header)?;
                    self.state = if header.last {
                        if self.current_header()?.has_checksum {
                            self.checksum_buffer = [0; 4];
                            State::Checksum { filled: 0 }
                        } else {
                            self.validate_frame_end()?;
                            State::FrameDone(FrameKind::Zstandard)
                        }
                    } else {
                        State::BlockHeader
                    };
                    if self.pending_len != 0 {
                        let start = self.pending_start;
                        let length = core::cmp::min(self.pending_len, self.history.len() - start);
                        self.pending_start = (start + length) % self.history.len();
                        self.pending_len -= length;
                        return Ok(InternalStep::Output {
                            consumed,
                            start,
                            length,
                        });
                    }
                }
                State::Checksum { filled } => {
                    let amount = core::cmp::min(4 - filled, input.len() - consumed);
                    self.checksum_buffer[filled..filled + amount]
                        .copy_from_slice(&input[consumed..consumed + amount]);
                    consumed += amount;
                    let filled = filled + amount;
                    if filled != 4 {
                        self.state = State::Checksum { filled };
                        return Ok(InternalStep::NeedInput { consumed });
                    }
                    let expected = u32::from_le_bytes(self.checksum_buffer);
                    let actual = self.checksum.digest() as u32;
                    if actual != expected {
                        return Err(DecodeError::ChecksumMismatch { expected, actual });
                    }
                    self.validate_frame_end()?;
                    self.state = State::FrameDone(FrameKind::Zstandard);
                }
                State::FrameDone(kind) => {
                    self.completed_frames = self
                        .completed_frames
                        .checked_add(1)
                        .ok_or(DecodeError::ArithmeticOverflow)?;
                    self.current_header = None;
                    self.state = State::Header;
                    return Ok(InternalStep::FrameFinished { consumed, kind });
                }
            }
        }
    }

    fn start_frame(&mut self, header: FrameHeader) -> Result<(), DecodeError> {
        if header.dictionary_id != 0 {
            return Err(DecodeError::UnsupportedDictionary {
                id: header.dictionary_id,
            });
        }
        let window =
            usize::try_from(header.window_size).map_err(|_| DecodeError::WindowTooLarge)?;
        if window > self.history.len() {
            return Err(DecodeError::HistoryTooSmall {
                required: window,
                provided: self.history.len(),
            });
        }
        self.current_header = Some(header);
        self.block_limit = core::cmp::min(window, MAX_BLOCK_SIZE);
        self.frame_output = 0;
        self.pending_len = 0;
        self.offsets = [1, 4, 8];
        self.checksum = XxHash64::new();
        self.huffman = HuffmanTable::new();
        self.literal_lengths = FseTable::new();
        self.offsets_table = FseTable::new();
        self.match_lengths = FseTable::new();
        Ok(())
    }

    fn current_header(&self) -> Result<FrameHeader, DecodeError> {
        self.current_header.ok_or(DecodeError::InvalidFrameHeader)
    }

    fn process_block(&mut self, header: BlockHeader) -> Result<(), DecodeError> {
        let start = self.history_position;
        let before = self.frame_output;
        match header.kind {
            BlockKind::Raw => {
                for index in 0..header.size {
                    self.write_byte(self.block[index])?;
                }
            }
            BlockKind::Rle => {
                let byte = self.block[0];
                for _ in 0..header.size {
                    self.write_byte(byte)?;
                }
            }
            BlockKind::Compressed => self.decode_compressed(header.size)?,
        }
        let produced = usize::try_from(self.frame_output - before)
            .map_err(|_| DecodeError::ArithmeticOverflow)?;
        if produced > self.block_limit {
            return Err(DecodeError::InvalidBlock);
        }
        self.record_output(start, produced);
        Ok(())
    }

    fn decode_compressed(&mut self, size: usize) -> Result<(), DecodeError> {
        let (literal_count, consumed) = decode_literals(
            &self.block[..size],
            self.literals,
            &mut self.huffman,
            self.block_limit,
        )?;
        let sequence_input = &self.block[consumed..size];
        let (sequence_count, mut position) = parse_sequence_count(sequence_input)?;
        if sequence_count == 0 {
            if position != sequence_input.len() {
                return Err(DecodeError::InvalidBlock);
            }
            let window_size = self.current_header()?.window_size;
            let mut writer = HistoryWriter {
                history: self.history,
                position: &mut self.history_position,
                frame_output: &mut self.frame_output,
                window_size,
            };
            for byte in &self.literals[..literal_count] {
                writer.write(*byte)?;
            }
            return Ok(());
        }
        let modes = *sequence_input
            .get(position)
            .ok_or(DecodeError::InvalidBlock)?;
        position += 1;
        if modes & 3 != 0 {
            return Err(DecodeError::InvalidBlock);
        }
        position += build_sequence_table(
            &mut self.literal_lengths,
            modes >> 6,
            &sequence_input[position..],
            &LL_DEFAULT,
            6,
            35,
            9,
        )?;
        position += build_sequence_table(
            &mut self.offsets_table,
            (modes >> 4) & 3,
            &sequence_input[position..],
            &OF_DEFAULT,
            5,
            31,
            8,
        )?;
        position += build_sequence_table(
            &mut self.match_lengths,
            (modes >> 2) & 3,
            &sequence_input[position..],
            &ML_DEFAULT,
            6,
            52,
            9,
        )?;
        let mut bits = BackwardBits::new(
            sequence_input
                .get(position..)
                .ok_or(DecodeError::InvalidBitstream)?,
        )?;
        let mut ll_state = bits.read(self.literal_lengths.log())?;
        let mut of_state = bits.read(self.offsets_table.log())?;
        let mut ml_state = bits.read(self.match_lengths.log())?;
        let mut literal_position = 0usize;
        let window_size = self.current_header()?.window_size;
        let literal_lengths = &self.literal_lengths;
        let offsets_table = &self.offsets_table;
        let match_lengths = &self.match_lengths;
        let literals = &self.literals[..literal_count];
        let repeated_offsets = &mut self.offsets;
        let mut writer = HistoryWriter {
            history: self.history,
            position: &mut self.history_position,
            frame_output: &mut self.frame_output,
            window_size,
        };

        for sequence in 0..sequence_count {
            let ll_code = literal_lengths.symbol(ll_state)? as usize;
            let of_code = offsets_table.symbol(of_state)?;
            let ml_code = match_lengths.symbol(ml_state)? as usize;
            if ll_code >= LL_BASE.len() || ml_code >= ML_BASE.len() || of_code > 31 {
                return Err(DecodeError::InvalidEntropyTable);
            }
            let raw_offset = (1u32 << of_code)
                .checked_add(bits.read(of_code)?)
                .ok_or(DecodeError::ArithmeticOverflow)?;
            let match_length = ML_BASE[ml_code]
                .checked_add(bits.read(ML_BITS[ml_code])?)
                .ok_or(DecodeError::ArithmeticOverflow)?;
            let literal_length = LL_BASE[ll_code]
                .checked_add(bits.read(LL_BITS[ll_code])?)
                .ok_or(DecodeError::ArithmeticOverflow)?;
            let literal_end = literal_position
                .checked_add(literal_length as usize)
                .ok_or(DecodeError::ArithmeticOverflow)?;
            if literal_end > literal_count {
                return Err(DecodeError::InvalidBlock);
            }
            for byte in &literals[literal_position..literal_end] {
                writer.write(*byte)?;
            }
            literal_position = literal_end;
            let offset = resolve_offset(raw_offset, literal_length, repeated_offsets)?;
            writer.copy_match(offset as usize, match_length as usize)?;

            if sequence + 1 != sequence_count {
                literal_lengths.update(&mut ll_state, &mut bits)?;
                match_lengths.update(&mut ml_state, &mut bits)?;
                offsets_table.update(&mut of_state, &mut bits)?;
            }
        }
        if bits.remaining() != 0 {
            return Err(DecodeError::InvalidBitstream);
        }
        for byte in &literals[literal_position..literal_count] {
            writer.write(*byte)?;
        }
        Ok(())
    }

    fn write_byte(&mut self, byte: u8) -> Result<(), DecodeError> {
        write_history_byte(
            self.history,
            &mut self.history_position,
            &mut self.frame_output,
            byte,
        )
    }

    fn record_output(&mut self, start: usize, length: usize) {
        if length == 0 {
            return;
        }
        let first = core::cmp::min(length, self.history.len() - start);
        self.checksum.update(&self.history[start..start + first]);
        if first != length {
            self.checksum.update(&self.history[..length - first]);
        }
        self.pending_start = start;
        self.pending_len = length;
    }

    fn validate_frame_end(&self) -> Result<(), DecodeError> {
        if let Some(expected) = self.current_header()?.content_size {
            if self.frame_output != expected {
                return Err(DecodeError::ContentSizeMismatch {
                    expected,
                    actual: self.frame_output,
                });
            }
        }
        Ok(())
    }
}

struct HistoryWriter<'a> {
    history: &'a mut [u8],
    position: &'a mut usize,
    frame_output: &'a mut u64,
    window_size: u64,
}

impl HistoryWriter<'_> {
    fn write(&mut self, byte: u8) -> Result<(), DecodeError> {
        write_history_byte(self.history, self.position, self.frame_output, byte)
    }

    fn copy_match(&mut self, offset: usize, length: usize) -> Result<(), DecodeError> {
        let available = core::cmp::min(*self.frame_output, self.window_size);
        if offset == 0 || offset as u64 > available {
            return Err(DecodeError::InvalidOffset);
        }
        for _ in 0..length {
            let source = (*self.position + self.history.len() - offset) % self.history.len();
            self.write(self.history[source])?;
        }
        Ok(())
    }
}

fn write_history_byte(
    history: &mut [u8],
    position: &mut usize,
    frame_output: &mut u64,
    byte: u8,
) -> Result<(), DecodeError> {
    if history.is_empty() {
        return Err(DecodeError::HistoryTooSmall {
            required: 1,
            provided: 0,
        });
    }
    history[*position] = byte;
    *position += 1;
    if *position == history.len() {
        *position = 0;
    }
    *frame_output = frame_output
        .checked_add(1)
        .ok_or(DecodeError::ArithmeticOverflow)?;
    Ok(())
}

pub fn inspect_frame(input: &[u8]) -> Result<HeaderStatus, DecodeError> {
    if input.len() < 4 {
        return Ok(HeaderStatus::NeedMore { minimum: 4 });
    }
    let magic = read_u32(input);
    if (SKIPPABLE_MAGIC_MIN..=SKIPPABLE_MAGIC_MAX).contains(&magic) {
        if input.len() < 8 {
            return Ok(HeaderStatus::NeedMore { minimum: 8 });
        }
        return Ok(HeaderStatus::Complete {
            header: StreamHeader::Skippable {
                magic,
                size: read_u32(&input[4..]),
            },
            size: 8,
        });
    }
    if magic != ZSTD_MAGIC {
        return Err(DecodeError::InvalidMagic);
    }
    if input.len() < 5 {
        return Ok(HeaderStatus::NeedMore { minimum: 5 });
    }
    let descriptor = input[4];
    if descriptor & 0x08 != 0 {
        return Err(DecodeError::InvalidFrameHeader);
    }
    let single_segment = descriptor & 0x20 != 0;
    let dictionary_size = [0usize, 1, 2, 4][(descriptor & 3) as usize];
    let content_flag = descriptor >> 6;
    let content_size_bytes = match content_flag {
        0 if single_segment => 1,
        0 => 0,
        1 => 2,
        2 => 4,
        _ => 8,
    };
    let size = 5 + usize::from(!single_segment) + dictionary_size + content_size_bytes;
    if input.len() < size {
        return Ok(HeaderStatus::NeedMore { minimum: size });
    }
    let mut position = 5usize;
    let encoded_window = if single_segment {
        None
    } else {
        let value = input[position];
        position += 1;
        let exponent = (value >> 3) as u32;
        let base = 1u64 << (10 + exponent);
        Some(base + (base / 8) * (value as u64 & 7))
    };
    let dictionary_id = read_variable(&input[position..position + dictionary_size]);
    position += dictionary_size;
    let content_size = if content_size_bytes == 0 {
        None
    } else {
        let mut value = read_variable_u64(&input[position..position + content_size_bytes]);
        if content_size_bytes == 2 {
            value += 256;
        }
        Some(value)
    };
    let window_size = if single_segment {
        content_size.ok_or(DecodeError::InvalidFrameHeader)?
    } else {
        encoded_window.ok_or(DecodeError::InvalidFrameHeader)?
    };
    Ok(HeaderStatus::Complete {
        header: StreamHeader::Zstandard(FrameHeader {
            window_size,
            content_size,
            dictionary_id,
            has_checksum: descriptor & 4 != 0,
        }),
        size,
    })
}

fn parse_block_header(bytes: [u8; 3], limit: usize) -> Result<BlockHeader, DecodeError> {
    let value = bytes[0] as u32 | (bytes[1] as u32) << 8 | (bytes[2] as u32) << 16;
    let kind = match (value >> 1) & 3 {
        0 => BlockKind::Raw,
        1 => BlockKind::Rle,
        2 => BlockKind::Compressed,
        _ => return Err(DecodeError::InvalidBlock),
    };
    let size = (value >> 3) as usize;
    if size > limit {
        return Err(DecodeError::InvalidBlock);
    }
    Ok(BlockHeader {
        last: value & 1 != 0,
        kind,
        size,
    })
}

fn decode_literals(
    input: &[u8],
    output: &mut [u8],
    table: &mut HuffmanTable,
    block_limit: usize,
) -> Result<(usize, usize), DecodeError> {
    let first = *input.first().ok_or(DecodeError::InvalidBlock)?;
    let kind = first & 3;
    let format = (first >> 2) & 3;
    if kind <= 1 {
        let (regenerated, header_size): (usize, usize) = match format {
            0 | 2 => ((first >> 3) as usize, 1),
            1 => {
                let second = *input.get(1).ok_or(DecodeError::InvalidBlock)?;
                (((first >> 4) as usize) | ((second as usize) << 4), 2)
            }
            _ => {
                let second = *input.get(1).ok_or(DecodeError::InvalidBlock)?;
                let third = *input.get(2).ok_or(DecodeError::InvalidBlock)?;
                (
                    ((first >> 4) as usize) | ((second as usize) << 4) | ((third as usize) << 12),
                    3,
                )
            }
        };
        ensure_literal_space(regenerated, output.len(), block_limit)?;
        if kind == 0 {
            let end = header_size
                .checked_add(regenerated)
                .ok_or(DecodeError::ArithmeticOverflow)?;
            let source = input
                .get(header_size..end)
                .ok_or(DecodeError::InvalidBlock)?;
            output[..regenerated].copy_from_slice(source);
            Ok((regenerated, end))
        } else {
            let value = *input.get(header_size).ok_or(DecodeError::InvalidBlock)?;
            output[..regenerated].fill(value);
            Ok((regenerated, header_size + 1))
        }
    } else {
        let (regenerated, compressed, streams, header_size): (usize, usize, usize, usize) =
            match format {
                0 | 1 => {
                    if input.len() < 3 {
                        return Err(DecodeError::InvalidBlock);
                    }
                    let combined =
                        input[0] as u32 | (input[1] as u32) << 8 | (input[2] as u32) << 16;
                    (
                        ((combined >> 4) & 0x3ff) as usize,
                        ((combined >> 14) & 0x3ff) as usize,
                        if format == 0 { 1 } else { 4 },
                        3,
                    )
                }
                2 => {
                    if input.len() < 4 {
                        return Err(DecodeError::InvalidBlock);
                    }
                    let combined = read_u32(input);
                    (
                        ((combined >> 4) & 0x3fff) as usize,
                        ((combined >> 18) & 0x3fff) as usize,
                        4,
                        4,
                    )
                }
                _ => {
                    if input.len() < 5 {
                        return Err(DecodeError::InvalidBlock);
                    }
                    let combined = read_variable_u64(&input[..5]);
                    (
                        ((combined >> 4) & 0x3ffff) as usize,
                        ((combined >> 22) & 0x3ffff) as usize,
                        4,
                        5,
                    )
                }
            };
        ensure_literal_space(regenerated, output.len(), block_limit)?;
        let end = header_size
            .checked_add(compressed)
            .ok_or(DecodeError::ArithmeticOverflow)?;
        let mut encoded = input
            .get(header_size..end)
            .ok_or(DecodeError::InvalidBlock)?;
        if kind == 2 {
            let table_size = table.read_description(encoded)?;
            encoded = &encoded[table_size..];
        } else if !table.is_valid() {
            return Err(DecodeError::InvalidEntropyTable);
        }
        if streams == 1 {
            table.decode(encoded, &mut output[..regenerated])?;
        } else {
            table.decode_four(encoded, &mut output[..regenerated])?;
        }
        Ok((regenerated, end))
    }
}

fn ensure_literal_space(
    required: usize,
    provided: usize,
    block_limit: usize,
) -> Result<(), DecodeError> {
    if required > block_limit {
        return Err(DecodeError::InvalidBlock);
    }
    if required > provided {
        return Err(DecodeError::LiteralScratchTooSmall { required, provided });
    }
    Ok(())
}

fn parse_sequence_count(input: &[u8]) -> Result<(usize, usize), DecodeError> {
    let first = *input.first().ok_or(DecodeError::InvalidBlock)? as usize;
    match first {
        0 => Ok((0, 1)),
        1..=127 => Ok((first, 1)),
        128..=254 => Ok((
            ((first - 128) << 8) + *input.get(1).ok_or(DecodeError::InvalidBlock)? as usize,
            2,
        )),
        _ => Ok((
            0x7f00
                + u16::from_le_bytes([
                    *input.get(1).ok_or(DecodeError::InvalidBlock)?,
                    *input.get(2).ok_or(DecodeError::InvalidBlock)?,
                ]) as usize,
            3,
        )),
    }
}

fn build_sequence_table(
    table: &mut FseTable,
    mode: u8,
    input: &[u8],
    predefined: &[i16],
    predefined_log: u8,
    max_symbol: usize,
    max_log: u8,
) -> Result<usize, DecodeError> {
    match mode {
        0 => {
            table.build(predefined, predefined_log)?;
            Ok(0)
        }
        1 => {
            table.build_rle(*input.first().ok_or(DecodeError::InvalidEntropyTable)?);
            Ok(1)
        }
        2 => table.read_description(input, max_symbol, max_log),
        3 if table.is_valid() => Ok(0),
        _ => Err(DecodeError::InvalidEntropyTable),
    }
}

fn resolve_offset(
    encoded: u32,
    literal_length: u32,
    repeated: &mut [u32; 3],
) -> Result<u32, DecodeError> {
    let value = if encoded > 3 {
        let value = encoded - 3;
        repeated[2] = repeated[1];
        repeated[1] = repeated[0];
        repeated[0] = value;
        value
    } else if literal_length != 0 {
        match encoded {
            1 => repeated[0],
            2 => {
                repeated.swap(0, 1);
                repeated[0]
            }
            3 => {
                let value = repeated[2];
                repeated[2] = repeated[1];
                repeated[1] = repeated[0];
                repeated[0] = value;
                value
            }
            _ => return Err(DecodeError::InvalidOffset),
        }
    } else {
        match encoded {
            1 => {
                repeated.swap(0, 1);
                repeated[0]
            }
            2 => {
                let value = repeated[2];
                repeated[2] = repeated[1];
                repeated[1] = repeated[0];
                repeated[0] = value;
                value
            }
            3 => {
                let value = repeated[0]
                    .checked_sub(1)
                    .filter(|value| *value != 0)
                    .ok_or(DecodeError::InvalidOffset)?;
                repeated[2] = repeated[1];
                repeated[1] = repeated[0];
                repeated[0] = value;
                value
            }
            _ => return Err(DecodeError::InvalidOffset),
        }
    };
    Ok(value)
}

fn read_u32(input: &[u8]) -> u32 {
    u32::from_le_bytes([input[0], input[1], input[2], input[3]])
}

fn read_variable(input: &[u8]) -> u32 {
    read_variable_u64(input) as u32
}

fn read_variable_u64(input: &[u8]) -> u64 {
    let mut value = 0u64;
    for (index, byte) in input.iter().enumerate() {
        value |= (*byte as u64) << (index * 8);
    }
    value
}

const LL_BASE: [u32; 36] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 18, 20, 22, 24, 28, 32, 40, 48, 64,
    128, 256, 512, 1024, 2048, 4096, 8192, 16384, 32768, 65536,
];
const LL_BITS: [u8; 36] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 3, 3, 4, 6, 7, 8, 9, 10, 11,
    12, 13, 14, 15, 16,
];
const ML_BASE: [u32; 53] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27,
    28, 29, 30, 31, 32, 33, 34, 35, 37, 39, 41, 43, 47, 51, 59, 67, 83, 99, 131, 259, 515, 1027,
    2051, 4099, 8195, 16387, 32771, 65539,
];
const ML_BITS: [u8; 53] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    1, 1, 1, 1, 2, 2, 3, 3, 4, 4, 5, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16,
];
const LL_DEFAULT: [i16; 36] = [
    4, 3, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 1, 1, 1, 2, 2, 2, 2, 2, 2, 2, 2, 2, 3, 2, 1, 1, 1, 1, 1,
    -1, -1, -1, -1,
];
const ML_DEFAULT: [i16; 53] = [
    1, 4, 3, 2, 2, 2, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, -1, -1, -1, -1, -1, -1, -1,
];
const OF_DEFAULT: [i16; 29] = [
    1, 1, 1, 1, 1, 1, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, -1, -1, -1, -1, -1,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sampled_datadog_header() {
        let bytes = [0x28, 0xb5, 0x2f, 0xfd, 0x04, 0x78];
        assert_eq!(
            inspect_frame(&bytes),
            Ok(HeaderStatus::Complete {
                header: StreamHeader::Zstandard(FrameHeader {
                    window_size: 32 * 1024 * 1024,
                    content_size: None,
                    dictionary_id: 0,
                    has_checksum: true,
                }),
                size: 6,
            })
        );
    }

    #[test]
    fn decodes_raw_frame_incrementally() {
        let frame = [
            0x28, 0xb5, 0x2f, 0xfd, 0x20, 0x05, // one-segment, size 5
            0x29, 0, 0, // last raw block, size 5
            b'h', b'e', b'l', b'l', b'o',
        ];
        let mut history = [0u8; 5];
        let mut block = [0u8; 5];
        let mut literals = [0u8; 5];
        let mut decoder = Decoder::new(DecoderBuffers {
            history: &mut history,
            block: &mut block,
            literals: &mut literals,
        });
        let mut input = &frame[..];
        let mut output = [0u8; 5];
        let mut output_len = 0;
        loop {
            let step = decoder.decode(input).unwrap();
            let consumed = step.consumed();
            input = &input[consumed..];
            match step {
                DecodeStep::Output { bytes, .. } => {
                    output[output_len..output_len + bytes.len()].copy_from_slice(bytes);
                    output_len += bytes.len();
                }
                DecodeStep::NeedInput { .. } if input.is_empty() => break,
                _ => {}
            }
        }
        assert_eq!(&output, b"hello");
        decoder.finish().unwrap();
    }

    #[test]
    fn decodes_rle_and_empty_frames() {
        let rle = [
            0x28, 0xb5, 0x2f, 0xfd, 0x20, 10, // one-segment, content size 10
            0x53, 0, 0, // last RLE block, regenerated size 10
            b'x',
        ];
        let mut history = [0u8; 10];
        let mut block = [0u8; 1];
        let mut literals = [];
        let mut decoder = Decoder::new(DecoderBuffers {
            history: &mut history,
            block: &mut block,
            literals: &mut literals,
        });
        let mut input = rle.as_slice();
        let mut output = [0u8; 10];
        let mut position = 0;
        loop {
            let step = decoder.decode(input).unwrap();
            let consumed = step.consumed();
            input = &input[consumed..];
            if let DecodeStep::Output { bytes, .. } = step {
                output[position..position + bytes.len()].copy_from_slice(bytes);
                position += bytes.len();
            } else if matches!(step, DecodeStep::NeedInput { .. }) {
                break;
            }
        }
        assert_eq!(output, [b'x'; 10]);
        decoder.finish().unwrap();

        let empty = [
            0x28, 0xb5, 0x2f, 0xfd, 0x20, 0, // one-segment, empty content
            1, 0, 0, // last raw block, size 0
        ];
        let mut history = [];
        let mut block = [];
        let mut literals = [];
        let mut decoder = Decoder::new(DecoderBuffers {
            history: &mut history,
            block: &mut block,
            literals: &mut literals,
        });
        let mut input = empty.as_slice();
        loop {
            let step = decoder.decode(input).unwrap();
            let consumed = step.consumed();
            input = &input[consumed..];
            if matches!(step, DecodeStep::NeedInput { .. }) {
                break;
            }
        }
        decoder.finish().unwrap();
    }
}
