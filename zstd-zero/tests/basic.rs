mod support;

use zstd_zero::*;

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
    let mut buffers = support::Buffers::new(5, 5, 5);
    let mut decoder = Decoder::new(buffers.as_decoder_buffers()).unwrap();
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
fn streams_fragmented_output_through_callback() {
    let frame = [
        0x28, 0xb5, 0x2f, 0xfd, 0x20, 0x05, 0x29, 0, 0, b'h', b'e', b'l', b'l', b'o',
    ];
    let mut buffers = support::Buffers::new(5, 5, 5);
    let mut decoder = Decoder::new(buffers.as_decoder_buffers()).unwrap();
    let mut output = Vec::new();
    for fragment in frame.chunks(2) {
        decoder
            .push(fragment, |bytes| {
                output.extend_from_slice(bytes);
                Ok::<_, ()>(())
            })
            .unwrap();
    }
    decoder
        .finish_with(|bytes| {
            output.extend_from_slice(bytes);
            Ok::<_, ()>(())
        })
        .unwrap();
    assert_eq!(output, b"hello");
}

#[test]
fn decodes_rle_and_empty_frames() {
    let rle = [
        0x28, 0xb5, 0x2f, 0xfd, 0x20, 10, // one-segment, content size 10
        0x53, 0, 0, // last RLE block, regenerated size 10
        b'x',
    ];
    let mut buffers = support::Buffers::new(10, 1, 0);
    let mut decoder = Decoder::new(buffers.as_decoder_buffers()).unwrap();
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
    let mut buffers = support::Buffers::new(0, 0, 0);
    let mut decoder = Decoder::new(buffers.as_decoder_buffers()).unwrap();
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

#[test]
fn short_table_storage_is_rejected_without_panicking() {
    for (fse_len, huffman_len, scratch_len) in [
        (0, HUFFMAN_ENTRIES, FSE_SCRATCH_LEN),
        (FSE_ENTRIES, 0, FSE_SCRATCH_LEN),
        (FSE_ENTRIES - 1, HUFFMAN_ENTRIES, FSE_SCRATCH_LEN),
        (FSE_ENTRIES, HUFFMAN_ENTRIES - 1, FSE_SCRATCH_LEN),
        (FSE_ENTRIES, HUFFMAN_ENTRIES, 0),
        (FSE_ENTRIES, HUFFMAN_ENTRIES, FSE_SCRATCH_LEN - 1),
    ] {
        let mut fse_scratch = std::vec![i16::MIN; scratch_len];
        let mut fse = std::vec![FseEntry::default(); fse_len];
        let mut huffman = std::vec![HuffmanEntry::default(); huffman_len];
        let result = Decoder::new(DecoderBuffers {
            history: &mut [],
            block: &mut [],
            literals: &mut [],
            fse_scratch: &mut fse_scratch,
            fse: &mut fse,
            huffman: &mut huffman,
        });
        assert!(matches!(result, Err(DecodeError::InvalidEntropyTable)));
    }
}
