# oci-zero

[![CI](https://github.com/pawelchcki/oci-zero/actions/workflows/ci.yml/badge.svg)](https://github.com/pawelchcki/oci-zero/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/oci-zero.svg)](https://crates.io/crates/oci-zero)
[![docs.rs](https://docs.rs/oci-zero/badge.svg)](https://docs.rs/oci-zero)

`oci-zero` aims to provide bounded-memory building blocks for accessing and
unpacking content from remote Open Container Initiative (OCI) registries.

> **Status:** This is an early project scaffold. Version 0.1.0 establishes the
> project constraints and roadmap but does not yet implement an OCI client or
> expose a functional public API.

## Goals

- Remain `no_std` and allocation-free by default.
- Parse OCI descriptors, manifests, indexes, and registry responses
  incrementally, using caller-owned buffers and borrowed data.
- Drive the OCI Distribution protocol through a caller-provided transport,
  without requiring a particular HTTP client, TLS stack, executor, or runtime.
- Verify content digests while blobs are streamed from a registry.
- Unpack image layers incrementally into a caller-provided sink, with bounded
  memory use and explicit handling for path safety and OCI whiteouts.
- Define a pluggable metadata store that can operate without a heap or a
  filesystem.
- Add optional `alloc` and `std` convenience adapters later without weakening
  the allocation-free core.

## Design direction

The core APIs will favor borrowed values, fixed-capacity caller-owned buffers,
visitors, and streaming source/sink traits. Network access, persistent storage,
decompression, and platform integration will be supplied by applications or by
optional adapter crates and features.

The intended implementation order is:

1. Streamed parsing for the OCI metadata needed by registry operations.
2. Transport-independent registry request and response state machines.
3. Digest verification and streamed layer unpacking.
4. A no-allocation metadata-store interface and reference backends.
5. Optional ergonomic adapters for environments with `alloc` or `std`.

## Compatibility

The minimum supported Rust version is 1.75. The default feature set is empty,
and the core crate is expected to compile for targets that do not provide the
Rust standard library.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE)), or
- MIT License ([LICENSE-MIT](LICENSE-MIT))

at your option.
