use std::fmt;
use std::io::{Cursor, Write};

use zstd_zero::{DecodeStep, Decoder, DecoderBuffers, HeaderStatus, StreamHeader, MAX_BLOCK_SIZE};

const DEFAULT_CASES: usize = 256;
const CONCATENATED_FRAMES: usize = 48;
const MAX_CASE_SIZE: usize = 1024 * 1024;
const BASE_SEED: u64 = 0x6a09_e667_f3bc_c909;

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

#[derive(Clone, Copy)]
struct FrameConfig {
    level: i32,
    strategy: zstd::zstd_safe::Strategy,
    checksum: bool,
    content_size: bool,
    window_log: u32,
    target_compressed_block_size: Option<u32>,
    max_write_fragment: usize,
    flush_every: Option<usize>,
    max_decode_fragment: usize,
}

struct CaseContext {
    number: usize,
    seed: u64,
    size: usize,
    mode: PayloadMode,
    config: FrameConfig,
}

impl fmt::Display for CaseContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "case={} seed={:#018x} size={} mode={:?} level={} strategy={:?} checksum={} \
             content_size={} window_log={} target_compressed_block_size={:?} write_fragment=1..={} \
             flush_every={:?} decode_fragment=1..={}",
            self.number,
            self.seed,
            self.size,
            self.mode,
            self.config.level,
            self.config.strategy,
            self.config.checksum,
            self.config.content_size,
            self.config.window_log,
            self.config.target_compressed_block_size,
            self.config.max_write_fragment,
            self.config.flush_every,
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

/// Base seed for the generated corpus.
///
/// Overridable so a scheduled run can explore a different corpus than the fixed
/// one CI replays on every push; a failing nightly reports its seed, and setting
/// this variable to that value reproduces the run exactly.
fn configured_base_seed() -> u64 {
    match std::env::var("ZSTD_ZERO_CROSSCHECK_SEED") {
        Ok(value) => value.parse::<u64>().unwrap_or_else(|error| {
            panic!("ZSTD_ZERO_CROSSCHECK_SEED must be an unsigned 64-bit integer: {error}")
        }),
        Err(std::env::VarError::NotPresent) => BASE_SEED,
        Err(error) => panic!("cannot read ZSTD_ZERO_CROSSCHECK_SEED: {error}"),
    }
}

fn case_seed(number: usize) -> u64 {
    configured_base_seed() ^ (number as u64).wrapping_mul(0xd134_2543_de82_ef95)
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
    sizes.extend([65_791, 65_792]);
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

fn config_for(number: usize, size: usize) -> FrameConfig {
    use zstd::zstd_safe::Strategy;

    const WRITE_FRAGMENTS: [usize; 7] = [1, 2, 7, 31, 257, 4096, 65_536];
    const DECODE_FRAGMENTS: [usize; 7] = [1, 2, 5, 17, 127, 1024, 8192];
    const FLUSH_INTERVALS: [Option<usize>; 4] = [None, Some(1), Some(3), Some(7)];
    const TARGET_BLOCK_SIZES: [Option<u32>; 5] = [
        None,
        Some(1024),
        Some(4096),
        Some(16 * 1024),
        Some(64 * 1024),
    ];
    const STRATEGIES: [Strategy; 9] = [
        Strategy::ZSTD_fast,
        Strategy::ZSTD_dfast,
        Strategy::ZSTD_greedy,
        Strategy::ZSTD_lazy,
        Strategy::ZSTD_lazy2,
        Strategy::ZSTD_btlazy2,
        Strategy::ZSTD_btopt,
        Strategy::ZSTD_btultra,
        Strategy::ZSTD_btultra2,
    ];

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

    FrameConfig {
        level: -7 + (number % 30) as i32,
        strategy: STRATEGIES[(number.wrapping_mul(5) + 1) % STRATEGIES.len()],
        checksum: number & 1 != 0,
        content_size: number & 2 != 0,
        window_log: 10 + (number.wrapping_mul(7) % 11) as u32,
        target_compressed_block_size: TARGET_BLOCK_SIZES[(number / 5) % TARGET_BLOCK_SIZES.len()],
        max_write_fragment,
        flush_every: FLUSH_INTERVALS[(number / 3) % FLUSH_INTERVALS.len()],
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

fn encode(payload: &[u8], context: &CaseContext) -> Vec<u8> {
    let config = context.config;
    let mut encoder = zstd::stream::Encoder::new(Vec::new(), config.level)
        .unwrap_or_else(|error| panic!("{context}: create encoder: {error}"));
    encoder
        .include_checksum(config.checksum)
        .unwrap_or_else(|error| panic!("{context}: set checksum: {error}"));
    encoder
        .include_contentsize(config.content_size)
        .unwrap_or_else(|error| panic!("{context}: set content-size flag: {error}"));
    encoder
        .window_log(config.window_log)
        .unwrap_or_else(|error| panic!("{context}: set window log: {error}"));
    encoder
        .set_parameter(zstd::zstd_safe::CParameter::Strategy(config.strategy))
        .unwrap_or_else(|error| panic!("{context}: set compression strategy: {error}"));
    encoder
        .set_target_cblock_size(config.target_compressed_block_size)
        .unwrap_or_else(|error| panic!("{context}: set target block size: {error}"));
    encoder
        .set_pledged_src_size(Some(payload.len() as u64))
        .unwrap_or_else(|error| panic!("{context}: pledge source size: {error}"));

    let mut random = SplitMix64::new(context.seed ^ 0xa409_3822_299f_31d0);
    let mut position = 0;
    let mut writes = 0;
    while position < payload.len() {
        let remaining = payload.len() - position;
        let amount = 1 + random.below(config.max_write_fragment.min(remaining));
        encoder
            .write_all(&payload[position..position + amount])
            .unwrap_or_else(|error| panic!("{context}: encoder write: {error}"));
        position += amount;
        writes += 1;
        if config
            .flush_every
            .is_some_and(|interval| writes % interval == 0)
        {
            encoder
                .flush()
                .unwrap_or_else(|error| panic!("{context}: encoder flush: {error}"));
        }
    }
    if payload.is_empty() && config.flush_every.is_some() {
        encoder
            .flush()
            .unwrap_or_else(|error| panic!("{context}: empty encoder flush: {error}"));
    }
    encoder
        .finish()
        .unwrap_or_else(|error| panic!("{context}: finish encoder: {error}"))
}

fn decode_upstream(compressed: &[u8], context: &impl fmt::Display) -> Vec<u8> {
    zstd::stream::decode_all(Cursor::new(compressed))
        .unwrap_or_else(|error| panic!("{context}: upstream decode: {error}"))
}

struct DecodeResult {
    output: Vec<u8>,
    started_frames: usize,
    finished_frames: usize,
}

fn decode_zero(
    compressed: &[u8],
    fragment_seed: u64,
    max_fragment: usize,
    history_size: usize,
    context: &impl fmt::Display,
) -> DecodeResult {
    let mut history = vec![0u8; history_size];
    let mut block = vec![0u8; MAX_BLOCK_SIZE];
    let mut literals = vec![0u8; MAX_BLOCK_SIZE];
    let mut decoder = Decoder::new(DecoderBuffers {
        history: &mut history,
        block: &mut block,
        literals: &mut literals,
    });
    let mut result = DecodeResult {
        output: Vec::new(),
        started_frames: 0,
        finished_frames: 0,
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
        loop {
            let available = input.len();
            let step = decoder
                .decode(input)
                .unwrap_or_else(|error| panic!("{context}: zstd-zero decode: {error:?}"));
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
                DecodeStep::FrameStarted { .. } => result.started_frames += 1,
                DecodeStep::Output { bytes, .. } => {
                    assert!(
                        !bytes.is_empty(),
                        "{context}: decoder returned empty output"
                    );
                    result.output.extend_from_slice(bytes);
                }
                DecodeStep::FrameFinished { .. } => result.finished_frames += 1,
            }
        }
        position = end;
        fragment_number += 1;
    }

    loop {
        match decoder
            .decode(&[])
            .unwrap_or_else(|error| panic!("{context}: final zstd-zero decode: {error:?}"))
        {
            DecodeStep::NeedInput { consumed } => {
                assert_eq!(consumed, 0, "{context}: empty input consumed bytes");
                break;
            }
            DecodeStep::FrameStarted { consumed, .. } => {
                assert_eq!(consumed, 0, "{context}: empty input consumed bytes");
                result.started_frames += 1;
            }
            DecodeStep::Output { consumed, bytes } => {
                assert_eq!(consumed, 0, "{context}: empty input consumed bytes");
                assert!(
                    !bytes.is_empty(),
                    "{context}: decoder returned empty output"
                );
                result.output.extend_from_slice(bytes);
            }
            DecodeStep::FrameFinished { consumed, .. } => {
                assert_eq!(consumed, 0, "{context}: empty input consumed bytes");
                result.finished_frames += 1;
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
        .unwrap_or_else(|error| panic!("{context}: zstd-zero finish: {error:?}"));
    result
}

fn declared_window(compressed: &[u8], context: &impl fmt::Display) -> usize {
    match zstd_zero::inspect_frame(compressed)
        .unwrap_or_else(|error| panic!("{context}: inspect encoded frame: {error:?}"))
    {
        HeaderStatus::Complete {
            header: StreamHeader::Zstandard(header),
            ..
        } => usize::try_from(header.window_size)
            .unwrap_or_else(|_| panic!("{context}: declared window does not fit usize")),
        HeaderStatus::Complete {
            header: StreamHeader::Skippable { .. },
            ..
        } => panic!("{context}: encoder unexpectedly produced a skippable frame"),
        HeaderStatus::NeedMore { minimum } => {
            panic!("{context}: encoded frame header needs at least {minimum} bytes")
        }
    }
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
    match std::env::var("ZSTD_ZERO_CROSSCHECK_CASES") {
        Ok(value) => value.parse::<usize>().unwrap_or_else(|error| {
            panic!("ZSTD_ZERO_CROSSCHECK_CASES must be a non-negative integer: {error}")
        }),
        Err(std::env::VarError::NotPresent) => DEFAULT_CASES,
        Err(error) => panic!("cannot read ZSTD_ZERO_CROSSCHECK_CASES: {error}"),
    }
}

#[test]
fn crosschecks_reference_frames() {
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
            config: config_for(number, size),
        };
        let expected = payload(mode, size, seed);
        let compressed = encode(&expected, &context);
        if number < target_sizes.len() && compressed.len() <= 64 * 1024 {
            context.config.max_decode_fragment = 1;
        }

        let upstream = decode_upstream(&compressed, &context);
        assert_bytes_equal(&upstream, &expected, "upstream", &context);

        let history_size = declared_window(&compressed, &context);
        let zero = decode_zero(
            &compressed,
            seed ^ 0x082e_fa98_ec4e_6c89,
            context.config.max_decode_fragment,
            history_size,
            &context,
        );
        assert_eq!(zero.started_frames, 1, "{context}: started frame count");
        assert_eq!(zero.finished_frames, 1, "{context}: finished frame count");
        assert_bytes_equal(&zero.output, &expected, "zstd-zero", &context);
    }
}

struct ConcatenatedContext {
    seed: u64,
    skippable_frames: usize,
}

impl fmt::Display for ConcatenatedContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "concatenated seed={:#018x} zstd_frames={} skippable_frames={}",
            self.seed, CONCATENATED_FRAMES, self.skippable_frames
        )
    }
}

fn append_skippable(output: &mut Vec<u8>, number: usize, seed: u64) {
    const SIZES: [usize; 8] = [0, 1, 2, 7, 31, 127, 257, 1024];
    let size = SIZES[number % SIZES.len()];
    let magic = 0x184d_2a50u32 + (number % 16) as u32;
    output.extend_from_slice(&magic.to_le_bytes());
    output.extend_from_slice(&(size as u32).to_le_bytes());
    let start = output.len();
    output.resize(start + size, 0);
    SplitMix64::new(seed).fill(&mut output[start..]);
}

#[test]
fn crosschecks_concatenated_frames_and_skippables() {
    let seed = BASE_SEED ^ 0xbb67_ae85_84ca_a73b;
    let sizes = target_sizes();
    let mut compressed = Vec::new();
    let mut expected = Vec::new();
    let mut skippable_frames = 0;

    for frame in 0..CONCATENATED_FRAMES {
        if frame % 3 == 0 {
            append_skippable(&mut compressed, frame, seed ^ frame as u64);
            skippable_frames += 1;
        }

        let number = 10_000 + frame;
        let frame_seed = case_seed(number);
        let size = sizes[(frame * 11) % sizes.len()].min(64 * 1024 + frame);
        let mode = PAYLOAD_MODES[frame % PAYLOAD_MODES.len()];
        let context = CaseContext {
            number,
            seed: frame_seed,
            size,
            mode,
            config: config_for(number, size),
        };
        let frame_payload = payload(mode, size, frame_seed);
        compressed.extend_from_slice(&encode(&frame_payload, &context));
        expected.extend_from_slice(&frame_payload);

        if frame % 7 == 0 {
            append_skippable(
                &mut compressed,
                frame + CONCATENATED_FRAMES,
                seed ^ !(frame as u64),
            );
            skippable_frames += 1;
        }
    }

    let context = ConcatenatedContext {
        seed,
        skippable_frames,
    };
    let upstream = decode_upstream(&compressed, &context);
    assert_bytes_equal(&upstream, &expected, "upstream", &context);

    let zero = decode_zero(
        &compressed,
        seed.rotate_left(23),
        997,
        MAX_CASE_SIZE,
        &context,
    );
    assert_eq!(
        zero.started_frames,
        CONCATENATED_FRAMES + skippable_frames,
        "{context}: started frame count"
    );
    assert_eq!(
        zero.finished_frames,
        CONCATENATED_FRAMES + skippable_frames,
        "{context}: finished frame count"
    );
    assert_bytes_equal(&zero.output, &expected, "zstd-zero", &context);
}
