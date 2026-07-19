# gzip-zero

`gzip-zero` is a `no_std`, allocation-free streaming gzip decoder. It wraps
the allocation-free core of `miniz_oxide` with incremental gzip header,
trailer, checksum, size, and concatenated-member handling.

The caller supplies the 32 KiB DEFLATE history buffer. Input may be split at
arbitrary byte boundaries and decoded output is returned as borrowed slices.

The crate requires Rust 1.75 or newer and contains no unsafe code.

Licensed under either Apache-2.0 or MIT, at your option.
