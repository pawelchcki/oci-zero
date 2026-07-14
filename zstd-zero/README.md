# zstd-zero

`zstd-zero` is an experimental, safe Rust decoder for standard Zstandard
frames. The crate is `no_std`, has no dependencies, and never allocates.

The caller supplies three reusable buffers:

- history at least as large as the frame's declared window;
- compressed-block scratch, up to 128 KiB;
- regenerated-literals scratch, up to 128 KiB.

Decoded bytes are returned as short-lived slices borrowed directly from the
history buffer. Dictionary, legacy, and magicless frames are not supported.

```rust
use zstd_zero::{DecodeStep, Decoder, DecoderBuffers, MAX_BLOCK_SIZE};

# let compressed = [0u8; 0];
let mut history = [0u8; 8 * 1024];
let mut block = [0u8; MAX_BLOCK_SIZE];
let mut literals = [0u8; MAX_BLOCK_SIZE];
let mut decoder = Decoder::new(DecoderBuffers {
    history: &mut history,
    block: &mut block,
    literals: &mut literals,
});

// Feed arbitrary input fragments. Every step reports how much input it used;
// consume an Output slice before calling the decoder again.
let step = decoder.decode(&compressed)?;
if let DecodeStep::Output { bytes, .. } = step {
    consume(bytes);
}
# fn consume(_: &[u8]) {}
# Ok::<(), zstd_zero::DecodeError>(())
```

At end-of-input, call `decode(&[])` until it returns `NeedInput`, then call
`finish()` to detect truncation. Output may have been observed before a final
content checksum fails, so callers that need atomic behavior must use a
transactional sink.

The crate is published separately so applications can use the decoder without
depending on OCI registry functionality.

The test suite compares against libzstd across compression levels and input
fragmentation patterns. CI also builds zstd 1.5.7's official `decodecorpus`
generator and checks 256 deterministic no-dictionary frames. The same check can
be run locally with:

```console
DECODECORPUS=/path/to/decodecorpus cargo test -p zstd-zero \
  --test conformance decodes_official_decodecorpus_frames -- --ignored
```
