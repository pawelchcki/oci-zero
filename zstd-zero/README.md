# zstd-zero

[![crates.io](https://img.shields.io/crates/v/zstd-zero.svg)](https://crates.io/crates/zstd-zero)
[![docs.rs](https://docs.rs/zstd-zero/badge.svg)](https://docs.rs/zstd-zero)

`zstd-zero` is an experimental, safe Rust decoder for standard Zstandard
frames. The crate is `no_std`, has no dependencies, and never allocates.

It requires Rust 1.75 or newer and contains no unsafe code
(`#![forbid(unsafe_code)]`).

```toml
[dependencies]
zstd-zero = "0.1"
```

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

## Testing

The test suite compares against libzstd across compression levels and input
fragmentation patterns. The differential suite generates 256 deterministic
frames across payload families, boundary sizes, compression levels and
strategies, frame and block options, exact declared history windows, and
streaming fragmentation, then decodes each with both libzstd and `zstd-zero`.
It also cross-checks a concatenated stream containing Zstandard and skippable
frames. Increase or reduce the generated case count with
`ZSTD_ZERO_CROSSCHECK_CASES`:

```console
ZSTD_ZERO_CROSSCHECK_CASES=1024 cargo test -p zstd-zero --test differential
```

That corpus is derived from a fixed base seed, so every push replays the same
256 frames. A nightly workflow runs a much larger corpus from a changing seed and
prints the seed it used; `ZSTD_ZERO_CROSSCHECK_SEED` reproduces such a run
exactly:

```console
ZSTD_ZERO_CROSSCHECK_CASES=25000 ZSTD_ZERO_CROSSCHECK_SEED=<seed> \
  cargo test -p zstd-zero --release --test differential
```

CI also builds zstd 1.5.7's official `decodecorpus` generator and checks 256
deterministic no-dictionary frames. The same check can be run locally with:

```console
DECODECORPUS=/path/to/decodecorpus cargo test -p zstd-zero \
  --test conformance decodes_official_decodecorpus_frames -- --ignored
```

Licensed under either Apache-2.0 or MIT, at your option.
