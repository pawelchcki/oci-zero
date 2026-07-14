use std::fmt;
use std::io::{Cursor, Read};

use flate2::read::MultiGzDecoder;
use flate2::{Compress, Compression, Crc, FlushCompress, Status};
use gzip_zero::{DecodeStep, Decoder, DecoderBuffers, MemberHeader, HISTORY_SIZE};

const DEFAULT_CASES: usize = 256;
const CONCATENATED_MEMBERS: usize = 48;
const MAX_CASE_SIZE: usize = 1024 * 1024;
const BASE_SEED: u64 = 0x510e_527f_ade6_82d1;
const HEADER_SEED_XOR: u64 = 0x1319_8a2e_0370_7344;
const WRITE_FRAGMENT_SEED_XOR: u64 = 0xa409_3822_299f_31d0;
const DECODE_FRAGMENT_SEED_XOR: u64 = 0x082e_fa98_ec4e_6c89;

#[derive(Clone, Copy, Debug)]
enum PayloadMode {
    Zeroed,
    RepeatedByte,
    ByteRamp,
    RandomBytes,
    LowEntropy,
    RepeatedMotif,
    StructuredRecords,
    PeriodicMutations,
}

const PAYLOAD_MODES: [PayloadMode; 8] = [
    PayloadMode::Zeroed,
    PayloadMode::RepeatedByte,
    PayloadMode::ByteRamp,
    PayloadMode::RandomBytes,
    PayloadMode::LowEntropy,
    PayloadMode::RepeatedMotif,
    PayloadMode::StructuredRecords,
    PayloadMode::PeriodicMutations,
];

#[derive(Clone, Copy, Debug)]
enum FlushSchedule {
    None,
    PartialEvery(usize),
    SyncEvery(usize),
    FullEvery(usize),
    MixedEvery(usize),
}

impl FlushSchedule {
    fn after_write(self, writes: usize) -> Option<FlushCompress> {
        let (interval, mode) = match self {
            Self::None => return None,
            Self::PartialEvery(interval) => (interval, FlushCompress::Partial),
            Self::SyncEvery(interval) => (interval, FlushCompress::Sync),
            Self::FullEvery(interval) => (interval, FlushCompress::Full),
            Self::MixedEvery(interval) => {
                let mode = match (writes / interval) % 3 {
                    0 => FlushCompress::Partial,
                    1 => FlushCompress::Sync,
                    _ => FlushCompress::Full,
                };
                (interval, mode)
            }
        };
        (writes % interval == 0).then_some(mode)
    }

    fn initial_flush(self) -> Option<FlushCompress> {
        match self {
            Self::None => None,
            Self::PartialEvery(_) => Some(FlushCompress::Partial),
            Self::SyncEvery(_) => Some(FlushCompress::Sync),
            Self::FullEvery(_) => Some(FlushCompress::Full),
            Self::MixedEvery(_) => Some(FlushCompress::Partial),
        }
    }
}

#[derive(Clone, Copy)]
struct HeaderConfig {
    extra_len: Option<usize>,
    filename_len: Option<usize>,
    comment_len: Option<usize>,
    mtime: u32,
    operating_system: u8,
    header_crc: bool,
    extra_flags: u8,
}

#[derive(Clone, Copy)]
struct MemberConfig {
    level: u32,
    window_bits: u8,
    header: HeaderConfig,
    max_write_fragment: usize,
    flush_schedule: FlushSchedule,
    max_decode_fragment: usize,
}

struct CaseContext {
    number: usize,
    seed: u64,
    size: usize,
    mode: PayloadMode,
    config: MemberConfig,
}

impl fmt::Display for CaseContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let header = self.config.header;
        write!(
            formatter,
            "case={} seed={:#018x} size={} mode={:?} level={} window_bits={} \
             header={{extra_len={:?}, filename_len={:?}, comment_len={:?}, mtime={}, os={}, \
             fhcrc={}, xfl={}}} write_fragments={{seed={:#018x}, size=1..={}}} flush={:?} \
             decode_fragments={{seed={:#018x}, size=1..={}}}",
            self.number,
            self.seed,
            self.size,
            self.mode,
            self.config.level,
            self.config.window_bits,
            header.extra_len,
            header.filename_len,
            header.comment_len,
            header.mtime,
            header.operating_system,
            header.header_crc,
            header.extra_flags,
            self.seed ^ WRITE_FRAGMENT_SEED_XOR,
            self.config.max_write_fragment,
            self.config.flush_schedule,
            self.seed ^ DECODE_FRAGMENT_SEED_XOR,
            self.config.max_decode_fragment,
        )
    }
}

struct SplitMix64(u64);

impl SplitMix64 {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut value = self.0;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        value ^ (value >> 31)
    }

    fn below(&mut self, upper: usize) -> usize {
        debug_assert!(upper != 0);
        (self.next_u64() % upper as u64) as usize
    }

    fn fill(&mut self, bytes: &mut [u8]) {
        for chunk in bytes.chunks_mut(8) {
            let random = self.next_u64().to_le_bytes();
            chunk.copy_from_slice(&random[..chunk.len()]);
        }
    }
}

fn case_seed(number: usize) -> u64 {
    BASE_SEED ^ (number as u64).wrapping_mul(0xd134_2543_de82_ef95)
}

fn target_sizes() -> Vec<usize> {
    let mut sizes: Vec<_> = (0..=18).collect();
    for power in 5..=20 {
        let size = 1usize << power;
        sizes.extend([size - 1, size]);
        if size < MAX_CASE_SIZE {
            sizes.push(size + 1);
        }
    }
    sizes.extend([65_534, 65_535, 65_536, 65_537]);
    sizes.sort_unstable();
    sizes.dedup();
    sizes
}

fn logarithmic_size(seed: u64) -> usize {
    let mut random = SplitMix64::new(seed ^ 0x243f_6a88_85a3_08d3);
    let bucket = random.below(21);
    let upper = 1usize << bucket;
    let lower = if bucket == 0 { 1 } else { upper / 2 };
    lower + random.below(upper - lower + 1)
}

fn config_for(number: usize, size: usize, seed: u64) -> MemberConfig {
    const WRITE_FRAGMENTS: [usize; 8] = [1, 2, 7, 31, 257, 4096, 32_768, 65_536];
    const DECODE_FRAGMENTS: [usize; 8] = [1, 2, 5, 17, 127, 1024, 8192, 65_536];
    const FLUSH_SCHEDULES: [FlushSchedule; 9] = [
        FlushSchedule::None,
        FlushSchedule::PartialEvery(1),
        FlushSchedule::PartialEvery(5),
        FlushSchedule::SyncEvery(1),
        FlushSchedule::SyncEvery(3),
        FlushSchedule::FullEvery(1),
        FlushSchedule::FullEvery(7),
        FlushSchedule::MixedEvery(2),
        FlushSchedule::MixedEvery(11),
    ];
    const EXTRA_LENGTHS: [usize; 7] = [0, 1, 2, 7, 31, 255, 1024];
    const TEXT_LENGTHS: [usize; 8] = [0, 1, 2, 7, 31, 127, 255, 1023];
    const OPERATING_SYSTEMS: [u8; 8] = [0, 3, 7, 10, 11, 13, 255, 42];

    let level = (number % 10) as u32;
    let mut max_write_fragment = WRITE_FRAGMENTS[(number / 2) % WRITE_FRAGMENTS.len()];
    let mut max_decode_fragment =
        DECODE_FRAGMENTS[(number.wrapping_mul(5) + 3) % DECODE_FRAGMENTS.len()];
    if size > 64 * 1024 {
        max_write_fragment = max_write_fragment.max(4096);
        max_decode_fragment = max_decode_fragment.max(257);
    }
    if size > 512 * 1024 {
        max_write_fragment = max_write_fragment.max(16 * 1024);
        max_decode_fragment = max_decode_fragment.max(1024);
    }

    MemberConfig {
        level,
        window_bits: 9 + (number.wrapping_mul(3) % 7) as u8,
        header: HeaderConfig {
            extra_len: (number & 1 != 0).then_some(EXTRA_LENGTHS[(number / 3) % 7]),
            filename_len: (number & 2 != 0).then_some(TEXT_LENGTHS[(number / 5) % 8]),
            comment_len: (number & 4 != 0).then_some(TEXT_LENGTHS[(number / 7 + 3) % 8]),
            mtime: if number % 5 == 0 { 0 } else { seed as u32 },
            operating_system: OPERATING_SYSTEMS[(number / 2) % OPERATING_SYSTEMS.len()],
            header_crc: number & 8 != 0,
            extra_flags: match level {
                1 => 4,
                9 => 2,
                _ => 0,
            },
        },
        max_write_fragment,
        flush_schedule: FLUSH_SCHEDULES[(number.wrapping_mul(5) + 1) % 9],
        max_decode_fragment,
    }
}

fn payload(mode: PayloadMode, size: usize, seed: u64) -> Vec<u8> {
    let mut random = SplitMix64::new(seed);
    match mode {
        PayloadMode::Zeroed => vec![0; size],
        PayloadMode::RepeatedByte => vec![random.next_u64() as u8; size],
        PayloadMode::ByteRamp => {
            const STEPS: [u8; 8] = [1, 2, 3, 7, 63, 127, 128, 255];
            let start = random.next_u64() as u8;
            let step = STEPS[random.below(STEPS.len())];
            (0..size)
                .map(|index| start.wrapping_add((index as u8).wrapping_mul(step)))
                .collect()
        }
        PayloadMode::RandomBytes => {
            let mut bytes = vec![0; size];
            random.fill(&mut bytes);
            bytes
        }
        PayloadMode::LowEntropy => {
            let alphabet = b"abcdefghijklmnopqrstuvwxyz012345";
            let alphabet_size = 2 + random.below(7);
            (0..size)
                .map(|_| alphabet[random.below(alphabet_size)])
                .collect()
        }
        PayloadMode::RepeatedMotif => {
            const MOTIF_SIZES: [usize; 19] = [
                1, 2, 3, 4, 7, 8, 15, 16, 31, 32, 63, 64, 127, 128, 255, 256, 1023, 1024, 4093,
            ];
            let motif_size = MOTIF_SIZES[random.below(MOTIF_SIZES.len())];
            let mut motif = vec![0; motif_size];
            random.fill(&mut motif);
            (0..size).map(|index| motif[index % motif_size]).collect()
        }
        PayloadMode::StructuredRecords => structured_records(size, seed),
        PayloadMode::PeriodicMutations => periodic_mutations(size, &mut random),
    }
}

fn structured_records(size: usize, seed: u64) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(size);
    let mut record = 0u64;
    while bytes.len() < size {
        bytes.extend_from_slice(b"record/");
        bytes.extend_from_slice(&record.to_le_bytes());
        bytes.extend_from_slice(&record.wrapping_mul(0x9e37_79b9).to_be_bytes());
        bytes.extend_from_slice(&(seed ^ record.rotate_left(17)).to_le_bytes());
        bytes.extend_from_slice(b"\0status=ready\n");
        record += 1;
    }
    bytes.truncate(size);
    bytes
}

fn periodic_mutations(size: usize, random: &mut SplitMix64) -> Vec<u8> {
    const PERIODS: [usize; 17] = [
        3, 4, 7, 8, 15, 16, 31, 32, 63, 64, 127, 128, 255, 256, 1023, 1024, 4093,
    ];
    const MUTATION_RUNS: [usize; 9] = [1, 2, 3, 4, 7, 8, 15, 16, 31];
    let period = PERIODS[random.below(PERIODS.len())];
    let mut pattern = vec![0; period];
    random.fill(&mut pattern);
    let mut bytes: Vec<_> = (0..size).map(|index| pattern[index % period]).collect();
    let mutation_period = 31 + random.below(4096);
    let mut position = random.below(mutation_period);
    while position < size {
        let run = MUTATION_RUNS[random.below(MUTATION_RUNS.len())];
        for byte in &mut bytes[position..size.min(position + run)] {
            *byte ^= 1 << random.below(8);
        }
        position += mutation_period;
    }
    bytes
}

fn header_bytes(config: HeaderConfig, seed: u64) -> Vec<u8> {
    const FHCRC: u8 = 0x02;
    const FEXTRA: u8 = 0x04;
    const FNAME: u8 = 0x08;
    const FCOMMENT: u8 = 0x10;

    let mut flags = 0;
    flags |= if config.header_crc { FHCRC } else { 0 };
    flags |= config.extra_len.map(|_| FEXTRA).unwrap_or(0);
    flags |= config.filename_len.map(|_| FNAME).unwrap_or(0);
    flags |= config.comment_len.map(|_| FCOMMENT).unwrap_or(0);

    let mut header = vec![0x1f, 0x8b, 8, flags];
    header.extend_from_slice(&config.mtime.to_le_bytes());
    header.extend_from_slice(&[config.extra_flags, config.operating_system]);

    let mut random = SplitMix64::new(seed ^ HEADER_SEED_XOR);
    if let Some(length) = config.extra_len {
        header.extend_from_slice(&(length as u16).to_le_bytes());
        let start = header.len();
        header.resize(start + length, 0);
        random.fill(&mut header[start..]);
    }
    if let Some(length) = config.filename_len {
        append_nonzero_text(&mut header, length, &mut random);
    }
    if let Some(length) = config.comment_len {
        append_nonzero_text(&mut header, length, &mut random);
    }
    if config.header_crc {
        let mut crc = Crc::new();
        crc.update(&header);
        header.extend_from_slice(&(crc.sum() as u16).to_le_bytes());
    }
    header
}

fn append_nonzero_text(output: &mut Vec<u8>, length: usize, random: &mut SplitMix64) {
    for _ in 0..length {
        output.push(1 + random.below(255) as u8);
    }
    output.push(0);
}

fn compress_input(
    compressor: &mut Compress,
    input: &[u8],
    flush: FlushCompress,
    output: &mut Vec<u8>,
    context: &impl fmt::Display,
) {
    let mut remaining = input;
    loop {
        let mut buffer = [0u8; 32 * 1024];
        let input_before = compressor.total_in();
        let output_before = compressor.total_out();
        let status = compressor
            .compress(remaining, &mut buffer, flush)
            .unwrap_or_else(|error| panic!("{context}: zlib encode: {error}"));
        let consumed = (compressor.total_in() - input_before) as usize;
        let written = (compressor.total_out() - output_before) as usize;
        assert!(
            consumed <= remaining.len(),
            "{context}: zlib consumed {consumed} bytes from {} bytes",
            remaining.len()
        );
        assert!(
            written <= buffer.len(),
            "{context}: zlib wrote {written} bytes into {} bytes",
            buffer.len()
        );
        output.extend_from_slice(&buffer[..written]);
        remaining = &remaining[consumed..];

        if status == Status::StreamEnd {
            assert_eq!(flush, FlushCompress::Finish, "{context}: early stream end");
            assert!(remaining.is_empty(), "{context}: finish left input");
            return;
        }
        assert!(
            consumed != 0 || written != 0 || status == Status::BufError,
            "{context}: zlib encoder stopped making progress"
        );

        if !remaining.is_empty() {
            continue;
        }
        match flush {
            FlushCompress::None => return,
            FlushCompress::Partial | FlushCompress::Sync | FlushCompress::Full
                if written < buffer.len() =>
            {
                // zlib has completed a non-finishing flush once it leaves output space.
                return;
            }
            FlushCompress::Finish => {
                assert_ne!(
                    status,
                    Status::BufError,
                    "{context}: zlib could not finish stream"
                );
            }
            _ if written == buffer.len() => {}
            _ => panic!("{context}: unsupported zlib flush result: {flush:?} {status:?}"),
        }
    }
}

fn encode(payload: &[u8], context: &CaseContext) -> Vec<u8> {
    let config = context.config;
    let mut output = header_bytes(config.header, context.seed);
    let mut compressor =
        Compress::new_with_window_bits(Compression::new(config.level), false, config.window_bits);
    let mut random = SplitMix64::new(context.seed ^ WRITE_FRAGMENT_SEED_XOR);
    let mut position = 0;
    let mut writes = 0;
    while position < payload.len() {
        let remaining = payload.len() - position;
        let amount = 1 + random.below(config.max_write_fragment.min(remaining));
        writes += 1;
        let flush = config
            .flush_schedule
            .after_write(writes)
            .unwrap_or(FlushCompress::None);
        compress_input(
            &mut compressor,
            &payload[position..position + amount],
            flush,
            &mut output,
            context,
        );
        position += amount;
    }
    if payload.is_empty() {
        if let Some(flush) = config.flush_schedule.initial_flush() {
            compress_input(&mut compressor, &[], flush, &mut output, context);
        }
    }
    compress_input(
        &mut compressor,
        &[],
        FlushCompress::Finish,
        &mut output,
        context,
    );

    let mut crc = Crc::new();
    crc.update(payload);
    output.extend_from_slice(&crc.sum().to_le_bytes());
    output.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    output
}

fn expected_header(config: HeaderConfig) -> MemberHeader {
    MemberHeader {
        modification_time: config.mtime,
        extra_flags: config.extra_flags,
        operating_system: config.operating_system,
    }
}

fn decode_upstream(compressed: &[u8], context: &impl fmt::Display) -> Vec<u8> {
    let mut output = Vec::new();
    MultiGzDecoder::new(Cursor::new(compressed))
        .read_to_end(&mut output)
        .unwrap_or_else(|error| panic!("{context}: upstream zlib decode: {error}"));
    output
}

struct DecodeResult {
    output: Vec<u8>,
    headers: Vec<MemberHeader>,
    finished_members: usize,
}

fn decode_zero(
    compressed: &[u8],
    fragment_seed: u64,
    max_fragment: usize,
    context: &impl fmt::Display,
) -> DecodeResult {
    let mut history = [0u8; HISTORY_SIZE];
    let mut decoder = Decoder::new(DecoderBuffers {
        history: &mut history,
    })
    .unwrap_or_else(|error| panic!("{context}: create gzip-zero decoder: {error:?}"));
    let mut result = DecodeResult {
        output: Vec::new(),
        headers: Vec::new(),
        finished_members: 0,
    };
    let mut random = SplitMix64::new(fragment_seed);
    let mut position = 0;
    let mut fragment_number = 0;
    let mut consumed_total = 0;

    while position < compressed.len() {
        let remaining = compressed.len() - position;
        let amount = if fragment_number == 0 {
            1
        } else {
            1 + random.below(max_fragment.min(remaining))
        };
        let end = position + amount;
        let mut input = &compressed[position..end];
        let mut turns = 0;
        loop {
            turns += 1;
            assert!(
                turns < 100_000,
                "{context}: decoder stopped making progress"
            );
            let available = input.len();
            let step = decoder
                .decode(input)
                .unwrap_or_else(|error| panic!("{context}: gzip-zero decode: {error:?}"));
            let consumed = step.consumed();
            assert!(
                consumed <= available,
                "{context}: decoder consumed {consumed} bytes from {available} bytes"
            );
            consumed_total += consumed;
            input = &input[consumed..];
            match step {
                DecodeStep::NeedInput { .. } => {
                    assert!(
                        input.is_empty(),
                        "{context}: NeedInput left {} bytes unconsumed",
                        input.len()
                    );
                    break;
                }
                DecodeStep::MemberStarted { header, .. } => result.headers.push(header),
                DecodeStep::Output { bytes, .. } => {
                    assert!(
                        !bytes.is_empty(),
                        "{context}: decoder returned empty output"
                    );
                    result.output.extend_from_slice(bytes);
                }
                DecodeStep::MemberFinished { .. } => result.finished_members += 1,
            }
        }
        position = end;
        fragment_number += 1;
    }

    loop {
        match decoder
            .decode(&[])
            .unwrap_or_else(|error| panic!("{context}: final gzip-zero decode: {error:?}"))
        {
            DecodeStep::NeedInput { consumed } => {
                assert_eq!(consumed, 0, "{context}: empty input consumed bytes");
                break;
            }
            DecodeStep::MemberStarted {
                consumed, header, ..
            } => {
                assert_eq!(consumed, 0, "{context}: empty input consumed bytes");
                result.headers.push(header);
            }
            DecodeStep::Output { consumed, bytes } => {
                assert_eq!(consumed, 0, "{context}: empty input consumed bytes");
                assert!(
                    !bytes.is_empty(),
                    "{context}: decoder returned empty output"
                );
                result.output.extend_from_slice(bytes);
            }
            DecodeStep::MemberFinished { consumed } => {
                assert_eq!(consumed, 0, "{context}: empty input consumed bytes");
                result.finished_members += 1;
            }
        }
    }
    assert_eq!(
        consumed_total,
        compressed.len(),
        "{context}: decoder consumed-byte total"
    );
    decoder
        .finish()
        .unwrap_or_else(|error| panic!("{context}: gzip-zero finish: {error:?}"));
    result
}

fn assert_bytes_equal(actual: &[u8], expected: &[u8], decoder: &str, context: &impl fmt::Display) {
    if actual == expected {
        return;
    }
    let difference = actual
        .iter()
        .zip(expected)
        .position(|(actual, expected)| actual != expected);
    panic!(
        "{context}: {decoder} output mismatch: actual_len={} expected_len={} \
         first_differing_byte={difference:?}",
        actual.len(),
        expected.len()
    );
}

fn configured_case_count() -> usize {
    match std::env::var("GZIP_ZERO_CROSSCHECK_CASES") {
        Ok(value) => value.parse::<usize>().unwrap_or_else(|error| {
            panic!("GZIP_ZERO_CROSSCHECK_CASES must be a non-negative integer: {error}")
        }),
        Err(std::env::VarError::NotPresent) => DEFAULT_CASES,
        Err(error) => panic!("cannot read GZIP_ZERO_CROSSCHECK_CASES: {error}"),
    }
}

#[test]
fn crosschecks_reference_members() {
    let target_sizes = target_sizes();
    for number in 0..configured_case_count() {
        let seed = case_seed(number);
        let size = target_sizes
            .get(number)
            .copied()
            .unwrap_or_else(|| logarithmic_size(seed));
        let mode = PAYLOAD_MODES[number % PAYLOAD_MODES.len()];
        let mut context = CaseContext {
            number,
            seed,
            size,
            mode,
            config: config_for(number, size, seed),
        };
        let expected = payload(mode, size, seed);
        let compressed = encode(&expected, &context);
        if number < target_sizes.len() && compressed.len() <= 128 * 1024 {
            context.config.max_decode_fragment = 1;
        }

        let upstream = decode_upstream(&compressed, &context);
        assert_bytes_equal(&upstream, &expected, "upstream zlib", &context);

        let zero = decode_zero(
            &compressed,
            seed ^ DECODE_FRAGMENT_SEED_XOR,
            context.config.max_decode_fragment,
            &context,
        );
        assert_eq!(
            zero.headers,
            [expected_header(context.config.header)],
            "{context}: member headers"
        );
        assert_eq!(zero.finished_members, 1, "{context}: finished member count");
        assert_bytes_equal(&zero.output, &expected, "gzip-zero", &context);
    }
}

struct ConcatenatedContext {
    seed: u64,
}

impl fmt::Display for ConcatenatedContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "concatenated seed={:#018x} members={} decode_fragment=1..=997",
            self.seed, CONCATENATED_MEMBERS
        )
    }
}

#[test]
fn crosschecks_concatenated_members() {
    let seed = BASE_SEED ^ 0xbb67_ae85_84ca_a73b;
    let sizes = target_sizes();
    let mut compressed = Vec::new();
    let mut expected = Vec::new();
    let mut expected_headers = Vec::new();

    for member in 0..CONCATENATED_MEMBERS {
        let number = 10_000 + member;
        let member_seed = case_seed(number);
        let size = sizes[(member * 11) % sizes.len()].min(96 * 1024 + member);
        let mode = PAYLOAD_MODES[member % PAYLOAD_MODES.len()];
        let context = CaseContext {
            number,
            seed: member_seed,
            size,
            mode,
            config: config_for(number, size, member_seed),
        };
        let member_payload = payload(mode, size, member_seed);
        compressed.extend_from_slice(&encode(&member_payload, &context));
        expected.extend_from_slice(&member_payload);
        expected_headers.push(expected_header(context.config.header));
    }

    let context = ConcatenatedContext { seed };
    let upstream = decode_upstream(&compressed, &context);
    assert_bytes_equal(&upstream, &expected, "upstream zlib", &context);

    let zero = decode_zero(&compressed, seed.rotate_left(23), 997, &context);
    assert_eq!(zero.headers, expected_headers, "{context}: member headers");
    assert_eq!(
        zero.finished_members, CONCATENATED_MEMBERS,
        "{context}: finished member count"
    );
    assert_bytes_equal(&zero.output, &expected, "gzip-zero", &context);
}
