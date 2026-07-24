# gzip-zero

[![crates.io](https://img.shields.io/crates/v/gzip-zero.svg)](https://crates.io/crates/gzip-zero)
[![docs.rs](https://docs.rs/gzip-zero/badge.svg)](https://docs.rs/gzip-zero)

`gzip-zero` is a `no_std`, allocation-free streaming gzip decoder. It wraps
the allocation-free core of `miniz_oxide` with incremental gzip header,
trailer, checksum, size, and concatenated-member handling.

The caller supplies the 32 KiB DEFLATE history buffer. Input may be split at
arbitrary byte boundaries and decoded output is returned as borrowed slices.

The crate requires Rust 1.75 or newer and contains no unsafe code
(`#![forbid(unsafe_code)]`).

```toml
[dependencies]
gzip-zero = "0.1"
```

The crate is `no_std` with an empty default feature set, so no `default-features`
opt-out is needed.

## Testing

The test suite uses upstream zlib as a test-only oracle. Its deterministic
differential test generates 256 gzip members across eight payload families;
sizes through 1 MiB around powers of two, the 32 KiB history window, and the
65,535-byte stored-block limit; compression levels 0-9; DEFLATE windows 9-15;
stored, fixed, and dynamic block opportunities; optional header fields; flush
schedules; and streaming fragmentation. It also checks a 48-member
concatenated stream. Increase or reduce the generated case count with
`GZIP_ZERO_CROSSCHECK_CASES`:

```console
GZIP_ZERO_CROSSCHECK_CASES=1024 cargo test -p gzip-zero --test differential
```

That corpus is derived from a fixed base seed, so every push replays the same
256 cases. A nightly workflow runs a much larger corpus from a changing seed and
prints the seed it used; `GZIP_ZERO_CROSSCHECK_SEED` reproduces such a run
exactly:

```console
GZIP_ZERO_CROSSCHECK_CASES=25000 GZIP_ZERO_CROSSCHECK_SEED=<seed> \
  cargo test -p gzip-zero --release --test differential
```

Licensed under either Apache-2.0 or MIT, at your option.
