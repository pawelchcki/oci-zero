use zstd_zero::{
    DecoderBuffers, FseEntry, HuffmanEntry, FSE_ENTRIES, FSE_SCRATCH_LEN, HUFFMAN_ENTRIES,
};

/// Own the initialized workspaces used by integration tests.
pub struct Buffers {
    history: Vec<u8>,
    block: Vec<u8>,
    literals: Vec<u8>,
    fse: Vec<FseEntry>,
    huffman: Vec<HuffmanEntry>,
    fse_scratch: [i16; FSE_SCRATCH_LEN],
}

impl Buffers {
    pub fn new(history: usize, block: usize, literals: usize) -> Self {
        Self {
            history: vec![0; history],
            block: vec![0; block],
            literals: vec![0; literals],
            fse: vec![FseEntry::new(); FSE_ENTRIES],
            huffman: vec![HuffmanEntry::new(); HUFFMAN_ENTRIES],
            fse_scratch: [0; FSE_SCRATCH_LEN],
        }
    }

    pub fn as_decoder_buffers(&mut self) -> DecoderBuffers<'_> {
        DecoderBuffers {
            history: &mut self.history,
            block: &mut self.block,
            literals: &mut self.literals,
            fse: &mut self.fse,
            huffman: &mut self.huffman,
            fse_scratch: &mut self.fse_scratch,
        }
    }
}
