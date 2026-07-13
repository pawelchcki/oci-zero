use zstd_zero::{DecodeError, DecodeStep, Decoder, DecoderBuffers, MAX_BLOCK_SIZE};

const RAW_FRAME: &[u8] = &[
    0x28, 0xb5, 0x2f, 0xfd, 0x20, 0x05, // one-segment, content size 5
    0x29, 0, 0, // last raw block, size 5
    b'h', b'e', b'l', b'l', b'o',
];

fn consume_to_boundary(decoder: &mut Decoder<'_>, mut input: &[u8]) -> Result<(), DecodeError> {
    loop {
        let step = decoder.decode(input)?;
        let consumed = step.consumed();
        input = &input[consumed..];
        if matches!(step, DecodeStep::NeedInput { .. }) {
            assert!(input.is_empty());
            return Ok(());
        }
    }
}

#[test]
fn every_raw_frame_truncation_is_detected() {
    for length in 0..RAW_FRAME.len() {
        let mut history = [0u8; 5];
        let mut block = [0u8; 5];
        let mut literals = [0u8; 5];
        let mut decoder = Decoder::new(DecoderBuffers {
            history: &mut history,
            block: &mut block,
            literals: &mut literals,
        });
        consume_to_boundary(&mut decoder, &RAW_FRAME[..length]).unwrap();
        assert_eq!(decoder.finish(), Err(DecodeError::UnexpectedEof));
    }
}

#[test]
fn reports_exact_caller_buffer_shortages() {
    let header = [0x28, 0xb5, 0x2f, 0xfd, 0x04, 0x78];
    let mut history = [0u8; 1024];
    let mut block = [0u8; 1];
    let mut literals = [0u8; 1];
    let mut decoder = Decoder::new(DecoderBuffers {
        history: &mut history,
        block: &mut block,
        literals: &mut literals,
    });
    assert_eq!(
        decoder.decode(&header).unwrap_err(),
        DecodeError::HistoryTooSmall {
            required: 32 * 1024 * 1024,
            provided: 1024,
        }
    );

    let mut history = [0u8; 5];
    let mut block = [0u8; 4];
    let mut literals = [0u8; 5];
    let mut decoder = Decoder::new(DecoderBuffers {
        history: &mut history,
        block: &mut block,
        literals: &mut literals,
    });
    let error = loop {
        match decoder.decode(RAW_FRAME) {
            Ok(DecodeStep::FrameStarted { consumed, .. }) => {
                match decoder.decode(&RAW_FRAME[consumed..]) {
                    Err(error) => break error,
                    Ok(_) => continue,
                }
            }
            Err(error) => break error,
            Ok(_) => continue,
        }
    };
    assert_eq!(
        error,
        DecodeError::BlockScratchTooSmall {
            required: 5,
            provided: 4,
        }
    );
}

#[test]
fn dictionary_and_reserved_headers_are_rejected() {
    let dictionary_frame = [
        0x28, 0xb5, 0x2f, 0xfd, 0x21, // single segment, one-byte dictionary ID
        13, 0, // dictionary 13, zero content size
    ];
    let mut history = [];
    let mut block = [];
    let mut literals = [];
    let mut decoder = Decoder::new(DecoderBuffers {
        history: &mut history,
        block: &mut block,
        literals: &mut literals,
    });
    assert_eq!(
        decoder.decode(&dictionary_frame).unwrap_err(),
        DecodeError::UnsupportedDictionary { id: 13 }
    );

    let reserved = [0x28, 0xb5, 0x2f, 0xfd, 0x28];
    assert_eq!(
        zstd_zero::inspect_frame(&reserved),
        Err(DecodeError::InvalidFrameHeader)
    );
    let unused = [0x28, 0xb5, 0x2f, 0xfd, 0x30, 0];
    assert!(matches!(
        zstd_zero::inspect_frame(&unused),
        Ok(zstd_zero::HeaderStatus::Complete { .. })
    ));
}

#[test]
fn arbitrary_fragmented_inputs_never_panic() {
    let mut random = 0x1234_5678_9abc_def0u64;
    for case in 0..2_000usize {
        let length = case % 513;
        let mut input = [0u8; 512];
        for byte in &mut input[..length] {
            random ^= random << 13;
            random ^= random >> 7;
            random ^= random << 17;
            *byte = random as u8;
        }
        let mut history = [0u8; 4096];
        let mut block = [0u8; 4096];
        let mut literals = [0u8; 4096];
        let mut decoder = Decoder::new(DecoderBuffers {
            history: &mut history,
            block: &mut block,
            literals: &mut literals,
        });
        let mut position = 0usize;
        while position < length {
            let end = core::cmp::min(position + 1 + case % 17, length);
            let mut chunk = &input[position..end];
            while let Ok(step) = decoder.decode(chunk) {
                let consumed = step.consumed();
                position += consumed;
                chunk = &chunk[consumed..];
                if matches!(step, DecodeStep::NeedInput { .. }) {
                    break;
                }
            }
            if decoder.decode(&[]).is_err() {
                break;
            }
        }
    }
}

#[test]
fn compressed_frame_reports_literal_scratch_requirement() {
    let expected = b"repeated test content ".repeat(10_000);
    let compressed = zstd::bulk::compress(&expected, 3).unwrap();
    let mut history = vec![0u8; expected.len()];
    let mut block = vec![0u8; MAX_BLOCK_SIZE];
    let mut literals = [];
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
        DecodeError::LiteralScratchTooSmall {
            required: 1..,
            provided: 0
        }
    ));
}

#[test]
fn mutations_of_a_valid_compressed_frame_never_panic() {
    let mut source = Vec::with_capacity(200_000);
    let mut random = 0xfeed_face_cafe_beefu64;
    for _ in 0..200_000 {
        random ^= random << 13;
        random ^= random >> 7;
        random ^= random << 17;
        source.push(random as u8);
    }
    let mut compressed = zstd::bulk::compress(&source, 9).unwrap();
    let mut history = vec![0u8; source.len()];
    let mut block = vec![0u8; MAX_BLOCK_SIZE];
    let mut literals = vec![0u8; MAX_BLOCK_SIZE];
    let mut decoder = Decoder::new(DecoderBuffers {
        history: &mut history,
        block: &mut block,
        literals: &mut literals,
    });

    for position in (0..compressed.len()).step_by(257) {
        compressed[position] ^= 0x40;
        let mut input = compressed.as_slice();
        while let Ok(step) = decoder.decode(input) {
            let consumed = step.consumed();
            input = &input[consumed..];
            if matches!(step, DecodeStep::NeedInput { .. }) {
                break;
            }
        }
        compressed[position] ^= 0x40;
        decoder.reset();
    }
}
