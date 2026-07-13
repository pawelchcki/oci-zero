use std::io::Write;
use std::process::Command;

use zstd_zero::{DecodeStep, Decoder, DecoderBuffers, MAX_BLOCK_SIZE};

fn decode_all(compressed: &[u8], chunk_size: usize) -> Vec<u8> {
    decode_with_history(compressed, chunk_size, 64 * 1024 * 1024)
}

fn decode_with_history(compressed: &[u8], chunk_size: usize, history_size: usize) -> Vec<u8> {
    let mut history = vec![0u8; history_size];
    let mut block = vec![0u8; MAX_BLOCK_SIZE];
    let mut literals = vec![0u8; MAX_BLOCK_SIZE];
    let mut decoder = Decoder::new(DecoderBuffers {
        history: &mut history,
        block: &mut block,
        literals: &mut literals,
    });
    let mut output = Vec::new();
    let mut position = 0usize;

    while position < compressed.len() {
        let end = (position + chunk_size).min(compressed.len());
        let mut input = &compressed[position..end];
        loop {
            let step = decoder.decode(input).unwrap();
            let consumed = step.consumed();
            position += consumed;
            input = &input[consumed..];
            match step {
                DecodeStep::Output { bytes, .. } => output.extend_from_slice(bytes),
                DecodeStep::NeedInput { .. } => {
                    assert!(input.is_empty());
                    break;
                }
                _ => {}
            }
        }
    }

    loop {
        match decoder.decode(&[]).unwrap() {
            DecodeStep::Output { bytes, .. } => output.extend_from_slice(bytes),
            DecodeStep::FrameStarted { .. } | DecodeStep::FrameFinished { .. } => {}
            DecodeStep::NeedInput { .. } => break,
        }
    }
    decoder.finish().unwrap();
    output
}

fn sample_data() -> Vec<u8> {
    let mut data = Vec::with_capacity(700_000);
    for index in 0..20_000u32 {
        data.extend_from_slice(b"etc/datadog-agent/conf.d/system_probe.d/conf.yaml\0");
        data.extend_from_slice(&index.to_le_bytes());
        data.extend_from_slice(&(index.wrapping_mul(2_654_435_761)).to_le_bytes());
        if index % 11 == 0 {
            data.extend(0u8..=255);
        }
    }
    data
}

#[test]
fn decodes_reference_frames_across_levels_and_chunking() {
    let expected = sample_data();
    for level in [-7, -5, 1, 3, 9, 19, 22] {
        let compressed = zstd::bulk::compress(&expected, level).unwrap();
        for chunk in [1, 7, 4096] {
            assert_eq!(
                decode_all(&compressed, chunk),
                expected,
                "level {level}, chunk {chunk}"
            );
        }
    }
}

#[test]
fn decodes_checksum_frame_without_content_size() {
    let expected = sample_data();
    let mut encoder = zstd::stream::Encoder::new(Vec::new(), 7).unwrap();
    encoder.include_checksum(true).unwrap();
    encoder.include_contentsize(false).unwrap();
    encoder.write_all(&expected).unwrap();
    let compressed = encoder.finish().unwrap();
    assert_eq!(decode_all(&compressed, 13), expected);
}

#[test]
fn decodes_concatenated_and_skippable_frames() {
    let first = b"first frame".repeat(1_000);
    let second = b"second frame".repeat(1_000);
    let mut compressed = zstd::bulk::compress(&first, 1).unwrap();
    compressed.extend_from_slice(&0x184d_2a55u32.to_le_bytes());
    compressed.extend_from_slice(&5u32.to_le_bytes());
    compressed.extend_from_slice(b"skip!");
    compressed.extend_from_slice(&zstd::bulk::compress(&second, 3).unwrap());
    let mut expected = first;
    expected.extend_from_slice(&second);
    assert_eq!(decode_all(&compressed, 2), expected);
}

#[test]
fn decodes_with_an_exact_wrapping_history_window() {
    let expected = b"abcdef0123456789".repeat(20_000);
    let mut encoder = zstd::stream::Encoder::new(Vec::new(), 5).unwrap();
    encoder.window_log(10).unwrap();
    encoder.include_contentsize(false).unwrap();
    encoder.write_all(&expected).unwrap();
    let compressed = encoder.finish().unwrap();
    assert_eq!(decode_with_history(&compressed, 31, 1 << 10), expected);
}

#[test]
fn rejects_corruption_and_poisoned_decoder() {
    let expected = sample_data();
    let mut encoder = zstd::stream::Encoder::new(Vec::new(), 3).unwrap();
    encoder.include_checksum(true).unwrap();
    encoder.write_all(&expected).unwrap();
    let mut compressed = encoder.finish().unwrap();
    *compressed.last_mut().unwrap() ^= 1;

    let mut history = vec![0u8; 64 * 1024 * 1024];
    let mut block = vec![0u8; MAX_BLOCK_SIZE];
    let mut literals = vec![0u8; MAX_BLOCK_SIZE];
    let mut decoder = Decoder::new(DecoderBuffers {
        history: &mut history,
        block: &mut block,
        literals: &mut literals,
    });
    let mut input = compressed.as_slice();
    let error = loop {
        match decoder.decode(input) {
            Ok(step) => {
                let consumed = step.consumed();
                input = &input[consumed..];
            }
            Err(error) => break error,
        }
    };
    assert!(matches!(
        error,
        zstd_zero::DecodeError::ChecksumMismatch { .. }
    ));
    assert_eq!(
        decoder.decode(&[]).unwrap_err(),
        zstd_zero::DecodeError::DecoderPoisoned
    );
}

#[test]
#[ignore = "requires DECODECORPUS pointing to zstd 1.5.7's decodecorpus binary"]
fn decodes_official_decodecorpus_frames() {
    let binary = std::env::var_os("DECODECORPUS").expect("set DECODECORPUS");
    let root = std::env::temp_dir().join(format!("zstd-zero-decodecorpus-{}", std::process::id()));
    let compressed = root.join("compressed");
    let original = root.join("original");
    std::fs::create_dir_all(&compressed).unwrap();
    std::fs::create_dir_all(&original).unwrap();
    let status = Command::new(binary)
        .arg(format!("-p{}", compressed.display()))
        .arg(format!("-o{}", original.display()))
        .args(["-s123456789", "-n256", "--max-content-size-log=18"])
        .status()
        .unwrap();
    assert!(status.success());

    for number in 0..256 {
        let name = format!("z{number:06}");
        let frame = std::fs::read(compressed.join(format!("{name}.zst"))).unwrap();
        let expected = std::fs::read(original.join(name)).unwrap();
        assert_eq!(
            decode_all(&frame, 1 + number % 257),
            expected,
            "corpus frame {number}"
        );
    }
    std::fs::remove_dir_all(root).unwrap();
}
