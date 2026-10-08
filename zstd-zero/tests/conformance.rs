use std::io::Write;
use std::process::Command;

mod support;

use zstd_zero::{Decoder, MAX_BLOCK_SIZE};

fn decode_all(compressed: &[u8], chunk_size: usize) -> Vec<u8> {
    decode_with_history(compressed, chunk_size, 64 * 1024 * 1024)
}

#[test]
fn accepts_custom_offset_alphabets_through_code_31() {
    // Each custom offset table assigns 31 slots to code 0 and one slot to
    // code 29, 30 or 31. The sequence selects code 0, so the fixture needs
    // a small window while exercising the entire table alphabet.
    for high_symbol in [0x3f, 0x7f, 0xbf] {
        let frame = [
            0x28,
            0xb5,
            0x2f,
            0xfd,
            0,
            0, // 1 KiB window, no declared content size
            0x65,
            0,
            0, // last compressed block, twelve bytes
            0x08,
            b'a', // one raw literal
            1,
            0x64,
            1, // one sequence; RLE LL=1, custom OF, RLE ML
            0xe0,
            0xf7,
            0xff,
            high_symbol,
            3, // offset FSE description
            0,
            0x20, // ML=3 and offset state 0 with an end marker
        ];
        assert_eq!(zstd::bulk::decompress(&frame, 4).unwrap(), b"aaaa");
        for chunk_size in [1, 2, frame.len()] {
            assert_eq!(decode_with_history(&frame, chunk_size, 1024), b"aaaa");
        }
    }
}

fn decode_with_history(compressed: &[u8], chunk_size: usize, history_size: usize) -> Vec<u8> {
    let mut buffers = support::Buffers::new(history_size, MAX_BLOCK_SIZE, MAX_BLOCK_SIZE);
    let mut decoder = Decoder::new(buffers.as_decoder_buffers()).unwrap();
    let mut output = Vec::new();
    let mut collect = |bytes: &[u8]| {
        output.extend_from_slice(bytes);
        Ok::<_, core::convert::Infallible>(())
    };
    for chunk in compressed.chunks(chunk_size) {
        decoder.push(chunk, &mut collect).unwrap();
    }
    decoder.finish_with(collect).unwrap();
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

    let mut buffers = support::Buffers::new(64 * 1024 * 1024, MAX_BLOCK_SIZE, MAX_BLOCK_SIZE);
    let mut decoder = Decoder::new(buffers.as_decoder_buffers()).unwrap();
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
