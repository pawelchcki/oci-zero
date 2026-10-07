use zstd_zero::{
    Decoder, DecoderBuffers, FseEntry, HuffmanEntry, FSE_ENTRIES, HUFFMAN_ENTRIES, MAX_BLOCK_SIZE,
};

#[test]
fn entropy_storage_survives_reset_poison_and_reconstruction() {
    let mut random = 0xCA517Au32;
    let entropy: Vec<u8> = (0..65536)
        .map(|_| {
            random ^= random << 13;
            random ^= random >> 17;
            random ^= random << 5;
            if random & 3 == 0 {
                (random >> 8) as u8
            } else {
                b"  etaoinshrdlu"[(random as usize >> 8) % 13]
            }
        })
        .collect();
    let payloads = [
        entropy,
        b"first alphabet and repeating sequence".repeat(2000),
        (0..64000).map(|i| ((i * 7 + i / 17) % 31) as u8).collect(),
    ];
    let frames: Vec<_> = payloads
        .iter()
        .map(|p| zstd::bulk::compress(p, 9).unwrap())
        .collect();
    let mut history = vec![0; MAX_BLOCK_SIZE];
    let mut block = vec![0; MAX_BLOCK_SIZE];
    let mut literals = vec![0; MAX_BLOCK_SIZE];
    // Extra storage is allowed and can be reused after dropping the decoder.
    let mut fse_scratch = [i16::MIN; zstd_zero::FSE_SCRATCH_LEN + 1];
    let mut fse = vec![FseEntry::new(); FSE_ENTRIES + 1];
    let mut huffman = vec![HuffmanEntry::new(); HUFFMAN_ENTRIES + 1];
    for _ in 0..2 {
        fse_scratch.fill(i16::MIN);
        let mut decoder = Decoder::new(DecoderBuffers {
            history: &mut history,
            block: &mut block,
            literals: &mut literals,
            fse_scratch: &mut fse_scratch,
            fse: &mut fse,
            huffman: &mut huffman,
        })
        .unwrap();
        for (frame, expected) in frames.iter().zip(&payloads) {
            decoder.reset();
            let mut output = Vec::new();
            for part in frame.chunks(7) {
                decoder
                    .push(part, |bytes| {
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
            assert_eq!(&output, expected);
            decoder.reset();
            assert!(decoder.decode(b"not zstd").is_err());
        }
        assert_eq!(fse_scratch[zstd_zero::FSE_SCRATCH_LEN], i16::MIN);
    }
}

#[test]
fn decoder_does_not_embed_entropy_workspaces() {
    // A stack-safety regression guard, not a stable public layout promise.
    assert!(std::mem::size_of::<Decoder<'_>>() <= 1024);
}
