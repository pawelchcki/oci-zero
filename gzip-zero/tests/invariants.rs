//! Invariant tests for edge cases the differential suite cannot reach.
//!
//! The differential suite cross-checks valid streams against a reference decoder.
//! These tests instead pin down invariants that hold for *malformed* input and at
//! exact ring-buffer boundaries — cases where a regression would otherwise pass CI.
use flate2::write::GzEncoder;
use flate2::{Compression, Crc};
use gzip_zero::{DecodeStep, Decoder, DecoderBuffers, HISTORY_SIZE};
use std::io::Write;

fn gz(data: &[u8]) -> Vec<u8> {
    let mut e = GzEncoder::new(Vec::new(), Compression::default());
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

/// Decode feeding exactly `chunk` bytes at a time; returns output and whether finish() was Ok.
fn decode_chunked(stream: &[u8], chunk: usize) -> (Vec<u8>, Result<(), String>) {
    let mut history = vec![0u8; HISTORY_SIZE];
    let mut decoder = Decoder::new(DecoderBuffers {
        history: &mut history,
    })
    .unwrap();
    let mut out = Vec::new();
    let mut pos = 0usize;
    let mut guard = 0usize;
    loop {
        guard += 1;
        assert!(guard < 50_000_000, "no-progress / infinite loop detected");
        let end = (pos + chunk).min(stream.len());
        let step = decoder.decode(&stream[pos..end]).expect("decode error");
        if let DecodeStep::Output { bytes, .. } = &step {
            assert!(!bytes.is_empty(), "Output step with empty slice");
            out.extend_from_slice(bytes);
        }
        pos += step.consumed();
        let done = matches!(step, DecodeStep::NeedInput { .. }) && pos == stream.len();
        if done {
            break;
        }
    }
    let fin = decoder.finish().map_err(|e| e.to_string());
    (out, fin)
}

#[test]
fn decodes_output_sizes_around_ring_boundary() {
    // Exercise output lengths straddling the 32 KiB ring wrap.
    for size in [
        0usize,
        1,
        HISTORY_SIZE - 1,
        HISTORY_SIZE,
        HISTORY_SIZE + 1,
        2 * HISTORY_SIZE,
        2 * HISTORY_SIZE + 1,
        3 * HISTORY_SIZE,
    ] {
        // Incompressible-ish payload so output length tracks input length.
        let payload: Vec<u8> = (0..size).map(|i| (i * 7 + i / 251) as u8).collect();
        let stream = gz(&payload);
        for chunk in [1usize, 3, 7, 4096, stream.len().max(1)] {
            let (out, fin) = decode_chunked(&stream, chunk);
            assert_eq!(out, payload, "size={size} chunk={chunk}");
            assert!(
                fin.is_ok(),
                "size={size} chunk={chunk}: finish() -> {fin:?}"
            );
        }
    }
}

#[test]
fn back_reference_before_member_start_cannot_leak() {
    // Member 1 emits a distinctive payload that must NOT be reachable from member 2.
    // Must exceed HISTORY_SIZE so the whole ring holds member-1 data; a short
    // member would leave the wrapped-to region still zero from Decoder::new.
    let secret = vec![b'B'; HISTORY_SIZE + 8192];
    let mut stream = gz(&secret);

    // Member 2: hand-crafted fixed-Huffman DEFLATE block:
    //   literal 'A', then match (len 3, dist 100) reaching *before* this member's output.
    // The window is zeroed at member start, so the match must resolve to zeros.
    let mut bits: Vec<u8> = Vec::new();
    let mut cur = 0u8;
    let mut nbits = 0u8;
    let push = |bit: u32, bits: &mut Vec<u8>, cur: &mut u8, nbits: &mut u8| {
        *cur |= ((bit & 1) as u8) << *nbits;
        *nbits += 1;
        if *nbits == 8 {
            bits.push(*cur);
            *cur = 0;
            *nbits = 0;
        }
    };
    let lsb = |v: u32, n: u32, bits: &mut Vec<u8>, cur: &mut u8, nbits: &mut u8| {
        for i in 0..n {
            push(v >> i, bits, cur, nbits);
        }
    };
    let msb = |v: u32, n: u32, bits: &mut Vec<u8>, cur: &mut u8, nbits: &mut u8| {
        for i in (0..n).rev() {
            push(v >> i, bits, cur, nbits);
        }
    };
    lsb(1, 1, &mut bits, &mut cur, &mut nbits); // BFINAL = 1
    lsb(1, 2, &mut bits, &mut cur, &mut nbits); // BTYPE = 01 (fixed)
    msb(0x30 + 65, 8, &mut bits, &mut cur, &mut nbits); // literal 'A'
    msb(1, 7, &mut bits, &mut cur, &mut nbits); // length code 257 => len 3
    msb(13, 5, &mut bits, &mut cur, &mut nbits); // distance code 13 => base 97
    lsb(3, 5, &mut bits, &mut cur, &mut nbits); // + 3 => dist 100
    msb(0, 7, &mut bits, &mut cur, &mut nbits); // end of block (256)
    if nbits > 0 {
        bits.push(cur);
    }

    let expected: Vec<u8> = vec![b'A', 0, 0, 0];
    let mut crc = Crc::new();
    crc.update(&expected);

    stream.extend_from_slice(&[0x1f, 0x8b, 0x08, 0x00, 0, 0, 0, 0, 0x00, 0xff]);
    stream.extend_from_slice(&bits);
    stream.extend_from_slice(&crc.sum().to_le_bytes());
    stream.extend_from_slice(&(expected.len() as u32).to_le_bytes());

    let (out, fin) = decode_chunked(&stream, 1);
    assert!(fin.is_ok(), "finish() -> {fin:?}");
    let member2 = &out[secret.len()..];
    assert_eq!(
        member2,
        &expected[..],
        "cross-member back-reference leaked previous member data"
    );
}

#[test]
fn empty_input_slices_are_idempotent() {
    let payload = b"hello world".to_vec();
    let stream = gz(&payload);
    let mut history = vec![0u8; HISTORY_SIZE];
    let mut decoder = Decoder::new(DecoderBuffers {
        history: &mut history,
    })
    .unwrap();
    // Repeated empty feeds must not advance state or panic.
    for _ in 0..100 {
        let step = decoder.decode(&[]).unwrap();
        assert_eq!(step.consumed(), 0);
    }
    let mut out = Vec::new();
    let mut pos = 0;
    loop {
        let step = decoder.decode(&stream[pos..]).unwrap();
        if let DecodeStep::Output { bytes, .. } = &step {
            out.extend_from_slice(bytes);
        }
        pos += step.consumed();
        if matches!(step, DecodeStep::NeedInput { .. }) && pos == stream.len() {
            break;
        }
    }
    assert_eq!(out, payload);
    assert!(decoder.finish().is_ok());
}
