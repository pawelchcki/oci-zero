#![doc = include_str!("../README.md")]
#![no_std]
#![forbid(unsafe_code)]

#[cfg(test)]
extern crate std;

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

pub use fse::Entry as FseEntry;
pub use huffman::Entry as HuffmanEntry;
/// FSE slots required for three sequence tables and Huffman weight scratch.
pub const FSE_ENTRIES: usize = 4 * 512;
/// Number of scratch counters shared by all FSE table constructors.
/// Contents may be arbitrary on entry and are not retained across tables.
pub const FSE_SCRATCH_LEN: usize = fse::MAX_SYMBOLS;
/// Huffman slots required for literal decoding.
pub const HUFFMAN_ENTRIES: usize = 2048;

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

/// A failure while driving a [`Decoder`] through its callback API.
#[derive(Debug, Eq, PartialEq, derive_more::Display)]
pub enum StreamError<E> {
    #[display("Zstandard decode failed: {_0}")]
    Decode(DecodeError),
    #[display("Zstandard output failed: {_0}")]
    Output(E),
    #[display("Zstandard decoder stopped making progress")]
    DecoderStalled,
}

enum InternalStep {
    NeedInput,
    FrameStarted(StreamHeader),
    Output { start: usize, length: usize },
    FrameFinished(FrameKind),
}

/// Initialized caller-owned storage, retained for the decoder's lifetime.
pub struct DecoderBuffers<'a> {
    /// At least [`FSE_SCRATCH_LEN`] elements for probabilities and next states.
    pub fse_scratch: &'a mut [i16],
    /// At least [`FSE_ENTRIES`] entries, including Huffman weight scratch.
    pub fse: &'a mut [FseEntry],
    /// At least [`HUFFMAN_ENTRIES`] entries.
    pub huffman: &'a mut [HuffmanEntry],
    pub history: &'a mut [u8],
    pub block: &'a mut [u8],
    pub literals: &'a mut [u8],
}

/// Tuning knobs for a [`Decoder`].
///
/// [`Default`] enables every validation check; prefer it unless you have a
/// specific reason not to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DecoderOptions {
    /// Reject Huffman literal streams that do not end exactly on their final bit.
    ///
    /// Enabled by default. Disabling it makes the decoder tolerate a truncated
    /// literal bitstream by padding the remaining symbols instead of failing,
    /// which is only useful for best-effort salvage of damaged data.
    ///
    /// # Warning
    ///
    /// With this disabled, a corrupt frame can decode "successfully" to bytes
    /// that differ from what other zstd implementations produce, with no error
    /// reported. Never disable it for content-addressed or otherwise
    /// security-sensitive input.
    pub strict_literal_bitstream: bool,
}

impl Default for DecoderOptions {
    fn default() -> Self {
        Self {
            strict_literal_bitstream: true,
        }
    }
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

impl BlockHeader {
    fn payload_size(self) -> usize {
        if self.kind == BlockKind::Rle {
            1
        } else {
            self.size
        }
    }
}

// Consume only the bytes needed by this state, retaining any following frame.
fn fill_buffer(buffer: &mut [u8], filled: &mut usize, input: &mut &[u8]) -> bool {
    let amount = (buffer.len() - *filled).min(input.len());
    buffer[*filled..*filled + amount].copy_from_slice(&input[..amount]);
    *filled += amount;
    *input = &input[amount..];
    *filled == buffer.len()
}

pub struct Decoder<'a> {
    fse_scratch: &'a mut [i16],
    options: DecoderOptions,
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
    huffman: HuffmanTable<'a>,
    literal_lengths: FseTable<'a>,
    offsets_table: FseTable<'a>,
    match_lengths: FseTable<'a>,
}

impl<'a> Decoder<'a> {
    /// Create a decoder with every validation check enabled.
    ///
    /// Returns [`DecodeError::InvalidEntropyTable`] if entropy storage is too short.
    pub fn new(buffers: DecoderBuffers<'a>) -> Result<Self, DecodeError> {
        Self::with_options(buffers, DecoderOptions::default())
    }

    /// Create a decoder with explicit [`DecoderOptions`].
    ///
    /// Returns [`DecodeError::InvalidEntropyTable`] if entropy storage is too short.
    pub fn with_options(
        buffers: DecoderBuffers<'a>,
        options: DecoderOptions,
    ) -> Result<Self, DecodeError> {
        if buffers.fse.len() < FSE_ENTRIES
            || buffers.huffman.len() < HUFFMAN_ENTRIES
            || buffers.fse_scratch.len() < FSE_SCRATCH_LEN
        {
            return Err(DecodeError::InvalidEntropyTable);
        }
        let (scratch, rest) = buffers.fse[..FSE_ENTRIES].split_at_mut(512);
        let (literal, rest) = rest.split_at_mut(512);
        let (offsets, matches) = rest.split_at_mut(512);
        Ok(Self {
            fse_scratch: &mut buffers.fse_scratch[..FSE_SCRATCH_LEN],
            options,
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
            checksum: XxHash64::new(0),
            huffman: HuffmanTable::new(
                &mut buffers.huffman[..HUFFMAN_ENTRIES],
                FseTable::new(scratch),
            ),
            literal_lengths: FseTable::new(literal),
            offsets_table: FseTable::new(offsets),
            match_lengths: FseTable::new(matches),
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
            InternalStep::FrameStarted(header) => DecodeStep::FrameStarted { consumed, header },
            InternalStep::Output { start, length } => DecodeStep::Output {
                consumed,
                bytes: &self.history[start..start + length],
            },
            InternalStep::FrameFinished(kind) => DecodeStep::FrameFinished { consumed, kind },
        })
    }

    /// Consumes an input fragment and sends decoded bytes to `output`.
    ///
    /// This is the convenient callback-driven counterpart to [`decode`](Self::decode).
    /// Frame events are handled internally while output errors remain distinct
    /// from compressed-stream errors.
    pub fn push<E>(
        &mut self,
        mut input: &[u8],
        mut output: impl FnMut(&[u8]) -> Result<(), E>,
    ) -> Result<(), StreamError<E>> {
        let mut idle_steps = 0;
        while !input.is_empty() {
            let step = self.decode(input).map_err(StreamError::Decode)?;
            let consumed = step.consumed();
            if consumed > input.len() {
                return Err(StreamError::DecoderStalled);
            }
            let needs_input = matches!(step, DecodeStep::NeedInput { .. });
            let produced = match step {
                DecodeStep::Output { bytes, .. } if !bytes.is_empty() => {
                    output(bytes).map_err(StreamError::Output)?;
                    true
                }
                _ => false,
            };
            input = &input[consumed..];
            if needs_input && !input.is_empty() {
                return Err(StreamError::DecoderStalled);
            }
            if consumed != 0 || produced {
                idle_steps = 0;
            } else {
                idle_steps += 1;
                if idle_steps > 16 {
                    return Err(StreamError::DecoderStalled);
                }
            }
        }
        Ok(())
    }

    /// Drains buffered output and validates the end of the stream.
    pub fn finish_with<E>(
        &mut self,
        mut output: impl FnMut(&[u8]) -> Result<(), E>,
    ) -> Result<(), StreamError<E>> {
        let mut idle_steps = 0;
        loop {
            match self.decode(&[]).map_err(StreamError::Decode)? {
                DecodeStep::NeedInput { .. } => break,
                DecodeStep::Output { bytes, .. } if !bytes.is_empty() => {
                    output(bytes).map_err(StreamError::Output)?;
                    idle_steps = 0;
                }
                _ => {
                    idle_steps += 1;
                    if idle_steps > 16 {
                        return Err(StreamError::DecoderStalled);
                    }
                }
            }
        }
        self.finish().map_err(StreamError::Decode)
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
        self.reset_frame();
    }

    fn reset_frame(&mut self) {
        self.frame_output = 0;
        self.pending_len = 0;
        self.offsets = [1, 4, 8];
        self.checksum = XxHash64::new(0);
        self.huffman.reset();
        self.literal_lengths.reset();
        self.offsets_table.reset();
        self.match_lengths.reset();
    }

    fn decode_inner(&mut self, mut input: &[u8]) -> Result<(usize, InternalStep), DecodeError> {
        let available = input.len();
        loop {
            let step = if self.pending_len != 0 {
                Some(self.pending_output())
            } else {
                self.advance(&mut input)?
            };
            if let Some(step) = step {
                return Ok((available - input.len(), step));
            }
        }
    }

    fn pending_output(&mut self) -> InternalStep {
        let start = self.pending_start;
        let length = self.pending_len.min(self.history.len() - start);
        self.pending_start = (start + length) % self.history.len();
        self.pending_len -= length;
        InternalStep::Output { start, length }
    }

    // None means a state transition; Some reports an event to the caller.
    fn advance(&mut self, input: &mut &[u8]) -> Result<Option<InternalStep>, DecodeError> {
        match self.state {
            State::Header => self.read_header(input),
            State::Skippable {
                magic,
                size,
                remaining,
            } => {
                let amount = remaining.min(input.len());
                *input = &input[amount..];
                let remaining = remaining - amount;
                if remaining == 0 {
                    self.state = State::FrameDone(FrameKind::Skippable { magic, size });
                    Ok(None)
                } else {
                    self.state = State::Skippable {
                        magic,
                        size,
                        remaining,
                    };
                    Ok(Some(InternalStep::NeedInput))
                }
            }
            State::BlockHeader => self.read_block_header(input),
            State::BlockPayload { header, filled } => self.read_block(input, header, filled),
            State::Checksum { filled } => self.read_checksum(input, filled),
            State::FrameDone(kind) => {
                self.completed_frames = self
                    .completed_frames
                    .checked_add(1)
                    .ok_or(DecodeError::ArithmeticOverflow)?;
                self.current_header = None;
                self.state = State::Header;
                Ok(Some(InternalStep::FrameFinished(kind)))
            }
        }
    }

    fn read_header(&mut self, input: &mut &[u8]) -> Result<Option<InternalStep>, DecodeError> {
        match inspect_frame(&self.header_buffer[..self.header_len])? {
            HeaderStatus::NeedMore { minimum } => {
                let complete = fill_buffer(
                    &mut self.header_buffer[..minimum],
                    &mut self.header_len,
                    input,
                );
                Ok((!complete).then_some(InternalStep::NeedInput))
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
                Ok(Some(InternalStep::FrameStarted(header)))
            }
        }
    }

    fn read_block_header(
        &mut self,
        input: &mut &[u8],
    ) -> Result<Option<InternalStep>, DecodeError> {
        if !fill_buffer(
            &mut self.block_header_buffer,
            &mut self.block_header_len,
            input,
        ) {
            return Ok(Some(InternalStep::NeedInput));
        }
        self.block_header_len = 0;
        let header = parse_block_header(self.block_header_buffer, self.block_limit)?;
        let required = header.payload_size();
        if required > self.block.len() {
            return Err(DecodeError::BlockScratchTooSmall {
                required,
                provided: self.block.len(),
            });
        }
        self.state = State::BlockPayload { header, filled: 0 };
        Ok(None)
    }

    fn read_block(
        &mut self,
        input: &mut &[u8],
        header: BlockHeader,
        mut filled: usize,
    ) -> Result<Option<InternalStep>, DecodeError> {
        if !fill_buffer(&mut self.block[..header.payload_size()], &mut filled, input) {
            self.state = State::BlockPayload { header, filled };
            return Ok(Some(InternalStep::NeedInput));
        }
        self.process_block(header)?;
        self.state = if !header.last {
            State::BlockHeader
        } else if self.current_header()?.has_checksum {
            self.checksum_buffer = [0; 4];
            State::Checksum { filled: 0 }
        } else {
            self.validate_frame_end()?;
            State::FrameDone(FrameKind::Zstandard)
        };
        Ok(None)
    }

    fn read_checksum(
        &mut self,
        input: &mut &[u8],
        mut filled: usize,
    ) -> Result<Option<InternalStep>, DecodeError> {
        if !fill_buffer(&mut self.checksum_buffer, &mut filled, input) {
            self.state = State::Checksum { filled };
            return Ok(Some(InternalStep::NeedInput));
        }
        let expected = u32::from_le_bytes(self.checksum_buffer);
        let actual = self.checksum.digest() as u32;
        if actual != expected {
            return Err(DecodeError::ChecksumMismatch { expected, actual });
        }
        self.validate_frame_end()?;
        self.state = State::FrameDone(FrameKind::Zstandard);
        Ok(None)
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
        self.reset_frame();
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
            self.fse_scratch,
            self.block_limit,
            self.options.strict_literal_bitstream,
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
        // Descriptions are serialized in literal-length, offset, match-length order.
        for (table, mode, predefined, log, max_log) in [
            (
                &mut self.literal_lengths,
                modes >> 6,
                LL_DEFAULT.as_slice(),
                6,
                9,
            ),
            (
                &mut self.offsets_table,
                (modes >> 4) & 3,
                OF_DEFAULT.as_slice(),
                5,
                8,
            ),
            (
                &mut self.match_lengths,
                (modes >> 2) & 3,
                ML_DEFAULT.as_slice(),
                6,
                9,
            ),
        ] {
            position += build_sequence_table(
                table,
                self.fse_scratch,
                mode,
                &sequence_input[position..],
                predefined,
                log,
                max_log,
            )?;
        }
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
            // Validated codes bound each sum to u32: offsets use at most 31
            // extra bits; lengths use at most 16. Literal positions and lengths
            // are bounded by the 128 KiB block limit, so their usize sum fits too.
            let raw_offset = (1u32 << of_code) + bits.read(of_code)?;
            let match_length = ML_BASE[ml_code] + bits.read(ML_BITS[ml_code])?;
            let literal_length = LL_BASE[ll_code] + bits.read(LL_BITS[ll_code])?;
            let literal_end = literal_position + literal_length as usize;
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
    let content_size_bytes = [usize::from(single_segment), 2, 4, 8][content_flag as usize];
    let size = 5 + usize::from(!single_segment) + dictionary_size + content_size_bytes;
    if input.len() < size {
        return Ok(HeaderStatus::NeedMore { minimum: size });
    }
    let mut position = 5 + usize::from(!single_segment);
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
        let value = input[5];
        let exponent = (value >> 3) as u32;
        let base = 1u64 << (10 + exponent);
        base + (base / 8) * (value as u64 & 7)
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
    table: &mut HuffmanTable<'_>,
    scratch: &mut [i16],
    block_limit: usize,
    strict: bool,
) -> Result<(usize, usize), DecodeError> {
    let first = *input.first().ok_or(DecodeError::InvalidBlock)?;
    let kind = first & 3;
    let format = (first >> 2) & 3;
    if kind <= 1 {
        let header_size = match format {
            0 | 2 => 1,
            1 => 2,
            _ => 3,
        };
        let packed = read_variable_u64(input.get(..header_size).ok_or(DecodeError::InvalidBlock)?);
        let regenerated = (packed >> if header_size == 1 { 3 } else { 4 }) as usize;
        ensure_literal_space(regenerated, output.len(), block_limit)?;
        return if kind == 0 {
            // The header is at most three bytes and the literal count was
            // checked against the 128 KiB block limit above.
            let end = header_size + regenerated;
            let source = input
                .get(header_size..end)
                .ok_or(DecodeError::InvalidBlock)?;
            output[..regenerated].copy_from_slice(source);
            Ok((regenerated, end))
        } else {
            let value = *input.get(header_size).ok_or(DecodeError::InvalidBlock)?;
            output[..regenerated].fill(value);
            Ok((regenerated, header_size + 1))
        };
    }
    let (header_size, size_bits) = match format {
        0 | 1 => (3, 10),
        2 => (4, 14),
        _ => (5, 18),
    };
    let packed = read_variable_u64(input.get(..header_size).ok_or(DecodeError::InvalidBlock)?);
    let mask = (1 << size_bits) - 1;
    let regenerated = ((packed >> 4) & mask) as usize;
    let compressed = ((packed >> (4 + size_bits)) & mask) as usize;
    ensure_literal_space(regenerated, output.len(), block_limit)?;
    // The encoded size is at most 18 bits; adding a five-byte header fits
    // in usize on every target that can address a 128 KiB block.
    let end = header_size + compressed;
    let mut encoded = input
        .get(header_size..end)
        .ok_or(DecodeError::InvalidBlock)?;
    if kind == 2 {
        let table_size = table.read_description(encoded, scratch)?;
        encoded = &encoded[table_size..];
    } else if !table.is_valid() {
        return Err(DecodeError::InvalidEntropyTable);
    }
    if format == 0 {
        table.decode(encoded, &mut output[..regenerated], strict)?;
    } else {
        table.decode_four(encoded, &mut output[..regenerated], strict)?;
    }
    Ok((regenerated, end))
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
    table: &mut FseTable<'_>,
    scratch: &mut [i16],
    mode: u8,
    input: &[u8],
    predefined: &[i16],
    predefined_log: u8,
    max_log: u8,
) -> Result<usize, DecodeError> {
    match mode {
        0 => {
            table.build(predefined, predefined_log, scratch)?;
            Ok(0)
        }
        1 => {
            table.build_rle(*input.first().ok_or(DecodeError::InvalidEntropyTable)?);
            Ok(1)
        }
        2 => table.read_description(input, predefined.len() - 1, max_log, scratch),
        3 if table.is_valid() => Ok(0),
        _ => Err(DecodeError::InvalidEntropyTable),
    }
}

fn resolve_offset(
    encoded: u32,
    literal_length: u32,
    repeated: &mut [u32; 3],
) -> Result<u32, DecodeError> {
    let index = match encoded {
        0 => return Err(DecodeError::InvalidOffset),
        1..=3 => encoded as usize - 1 + usize::from(literal_length == 0),
        _ => 2,
    };
    let value = if encoded > 3 {
        encoded - 3
    } else if index == 3 {
        repeated[0]
            .checked_sub(1)
            .filter(|value| *value != 0)
            .ok_or(DecodeError::InvalidOffset)?
    } else {
        repeated[index]
    };
    // A selected repeat moves to the front; a new offset evicts the oldest.
    repeated[..=index.min(2)].rotate_right(1);
    repeated[0] = value;
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
