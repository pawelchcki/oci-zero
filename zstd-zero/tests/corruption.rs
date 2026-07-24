//! Corrupted frames must never decode "successfully" to the wrong bytes.
//!
//! `robustness.rs` proves mutated frames do not panic; it does not check that
//! corruption is *detected*, nor that an accepted frame matches the reference.
//! The invariant pinned here is one-sided on purpose: this decoder may be
//! stricter than libzstd (rejecting streams libzstd tolerates), but whenever it
//! accepts a frame the output must be byte-identical to libzstd's.

use zstd_zero::{DecodeStep, Decoder, DecoderBuffers, DecoderOptions, MAX_BLOCK_SIZE};

/// Decode a complete stream. `Ok` only if the decoder accepted it cleanly.
fn decode_zero(compressed: &[u8], history_size: usize) -> Result<Vec<u8>, String> {
    decode_with(compressed, history_size, DecoderOptions::default())
}

fn decode_with(
    compressed: &[u8],
    history_size: usize,
    options: DecoderOptions,
) -> Result<Vec<u8>, String> {
    let mut history = vec![0u8; history_size];
    let mut block = vec![0u8; MAX_BLOCK_SIZE];
    let mut literals = vec![0u8; MAX_BLOCK_SIZE];
    let mut decoder = Decoder::with_options(
        DecoderBuffers {
            history: &mut history,
            block: &mut block,
            literals: &mut literals,
        },
        options,
    );
    let mut out = Vec::new();
    let mut input = compressed;
    let mut finished = 0usize;
    loop {
        let step = decoder
            .decode(input)
            .map_err(|error| format!("{error:?}"))?;
        let consumed = step.consumed();
        if consumed > input.len() {
            return Err("consumed more than was available".into());
        }
        if let DecodeStep::Output { bytes, .. } = &step {
            out.extend_from_slice(bytes);
        }
        if matches!(step, DecodeStep::FrameFinished { .. }) {
            finished += 1;
        }
        input = &input[consumed..];
        if matches!(step, DecodeStep::NeedInput { .. }) {
            if !input.is_empty() {
                return Err("stalled without consuming its input".into());
            }
            break;
        }
    }
    if finished == 0 {
        return Err("no frame finished".into());
    }
    Ok(out)
}

/// Text-like payload so literals use Huffman coding with a skewed distribution.
fn payload(size: usize) -> Vec<u8> {
    let words = [
        "the", "quick", "brown", "fox", "jumps", "over", "lazy", "dog", "and", "then", "runs",
        "away", "into", "forest", "where", "nothing", "happens", "at", "all", "today",
    ];
    let mut out = Vec::with_capacity(size + 16);
    let mut state = 0x1234_5678_9abc_def0u64;
    while out.len() < size {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.extend_from_slice(words[(state % words.len() as u64) as usize].as_bytes());
        out.push(b' ');
    }
    out
}

#[test]
fn accepted_corrupt_frames_match_the_reference_decoder() {
    let source = payload(12 * 1024);
    let clean = zstd::bulk::compress(&source, 9).unwrap();
    assert_eq!(
        decode_zero(&clean, source.len()).unwrap(),
        source,
        "clean frame must round-trip"
    );

    let mut divergences = Vec::new();
    let mut accepted = 0usize;

    for position in 0..clean.len() {
        for mask in [0x01u8, 0x40, 0x80] {
            let mut bad = clean.clone();
            bad[position] ^= mask;

            let ours = match decode_zero(&bad, source.len()) {
                // Rejecting is always allowed: this decoder may be stricter.
                Err(_) => continue,
                Ok(output) => output,
            };
            accepted += 1;

            match zstd::bulk::decompress(&bad, source.len() * 2) {
                Ok(theirs) if theirs == ours => {}
                Ok(theirs) => {
                    let first = ours.iter().zip(&theirs).position(|(a, b)| a != b);
                    divergences.push(format!(
                        "byte {position} ^ {mask:#04x}: accepted but differs from reference \
                         (ours {} bytes, reference {} bytes, first difference at {first:?})",
                        ours.len(),
                        theirs.len()
                    ));
                }
                Err(_) => divergences.push(format!(
                    "byte {position} ^ {mask:#04x}: accepted a frame the reference rejects \
                     ({} bytes)",
                    ours.len()
                )),
            }
        }
    }

    assert!(
        divergences.is_empty(),
        "{} of {accepted} accepted frames diverge from the reference decoder:\n  {}",
        divergences.len(),
        divergences
            .iter()
            .take(10)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n  ")
    );
}

/// Both modes must agree on valid input; only the lenient mode may accept a
/// truncated literal bitstream. This pins the option's contract in both
/// directions so the strict path cannot silently become the lenient one.
#[test]
fn strict_mode_rejects_what_lenient_mode_tolerates() {
    let source = payload(12 * 1024);
    let clean = zstd::bulk::compress(&source, 9).unwrap();
    let lenient = DecoderOptions {
        strict_literal_bitstream: false,
    };

    // Valid input: identical results either way.
    assert_eq!(decode_zero(&clean, source.len()).unwrap(), source);
    assert_eq!(
        decode_with(&clean, source.len(), lenient).unwrap(),
        source,
        "lenient mode must not change valid decoding"
    );

    // Find corruption that the lenient mode accepts but strict rejects, and
    // confirm the lenient output is in fact wrong.
    let mut lenient_only = 0usize;
    for position in 0..clean.len() {
        for mask in [0x01u8, 0x40, 0x80] {
            let mut bad = clean.clone();
            bad[position] ^= mask;
            let strict = decode_zero(&bad, source.len());
            let loose = decode_with(&bad, source.len(), lenient);
            if strict.is_err() {
                if let Ok(output) = loose {
                    lenient_only += 1;
                    assert_ne!(
                        output, source,
                        "byte {position} ^ {mask:#04x}: lenient mode accepted corruption \
                         yet reproduced the original bytes"
                    );
                }
            }
        }
    }
    assert!(
        lenient_only > 0,
        "expected lenient mode to accept some frames strict mode rejects; \
         the option may no longer have any effect"
    );
}
