use std::io::Write;

use flate2::write::GzEncoder;
use flate2::Compression;
use gzip_zero::{DecodeError, DecodeStep, Decoder, DecoderBuffers, HISTORY_SIZE};

fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

fn decode(input: &[u8], fragment: usize) -> Result<Vec<u8>, DecodeError> {
    let mut history = [0u8; HISTORY_SIZE];
    let mut decoder = Decoder::new(DecoderBuffers {
        history: &mut history,
    })?;
    let mut output = Vec::new();
    let mut position = 0;
    while position < input.len() {
        let end = (position + fragment).min(input.len());
        let mut chunk = &input[position..end];
        let mut turns = 0;
        while !chunk.is_empty() {
            turns += 1;
            assert!(turns < 100_000, "decoder stopped making progress");
            let step = decoder.decode(chunk)?;
            let consumed = step.consumed();
            if let DecodeStep::Output { bytes, .. } = &step {
                output.extend_from_slice(bytes);
            }
            assert!(consumed != 0 || !matches!(step, DecodeStep::NeedInput { .. }));
            chunk = &chunk[consumed..];
        }
        position = end;
    }
    decoder.finish()?;
    Ok(output)
}

#[test]
fn decodes_at_every_small_fragment_size() {
    let expected = (0..200_000)
        .map(|index| (index % 251) as u8)
        .collect::<Vec<_>>();
    let encoded = gzip(&expected);
    for fragment in 1..=64 {
        assert_eq!(
            decode(&encoded, fragment).unwrap(),
            expected,
            "fragment {fragment}"
        );
    }
}

#[test]
fn decodes_concatenated_members() {
    let mut encoded = gzip(b"first");
    encoded.extend_from_slice(&gzip(b"second"));
    assert_eq!(decode(&encoded, 3).unwrap(), b"firstsecond");
}

#[test]
fn rejects_checksum_corruption_and_poisoning() {
    let mut encoded = gzip(b"contents");
    let trailer = encoded.len() - 8;
    encoded[trailer] ^= 1;

    let mut history = [0u8; HISTORY_SIZE];
    let mut decoder = Decoder::new(DecoderBuffers {
        history: &mut history,
    })
    .unwrap();
    let error = loop {
        match decoder.decode(&encoded) {
            Ok(DecodeStep::Output { consumed, .. })
            | Ok(DecodeStep::MemberStarted { consumed, .. })
            | Ok(DecodeStep::MemberFinished { consumed })
            | Ok(DecodeStep::NeedInput { consumed }) => {
                encoded = encoded[consumed..].to_vec();
            }
            Err(error) => break error,
        }
    };
    assert!(matches!(error, DecodeError::InvalidDataChecksum { .. }));
    assert_eq!(decoder.decode(&[]), Err(DecodeError::DecoderPoisoned));
}

#[test]
fn reports_truncation_and_bad_history() {
    let encoded = gzip(b"contents");
    assert_eq!(
        decode(&encoded[..encoded.len() - 1], 7),
        Err(DecodeError::UnexpectedEof)
    );

    let mut short = [0u8; HISTORY_SIZE - 1];
    assert!(matches!(
        Decoder::new(DecoderBuffers {
            history: &mut short
        }),
        Err(DecodeError::InvalidHistorySize { .. })
    ));
}
