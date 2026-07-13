# oci-zero

[![CI](https://github.com/pawelchcki/oci-zero/actions/workflows/ci.yml/badge.svg)](https://github.com/pawelchcki/oci-zero/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/oci-zero.svg)](https://crates.io/crates/oci-zero)
[![docs.rs](https://docs.rs/oci-zero/badge.svg)](https://docs.rs/oci-zero)

`oci-zero` aims to provide bounded-memory building blocks for accessing and
unpacking content from remote Open Container Initiative (OCI) registries.

> [!WARNING]
> **Work in progress:** Version 0.0.1 does not yet implement an OCI client. Its
> first usable core primitive is an allocation-free streaming tar entry
> extractor.

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
and platform integration will be supplied by applications or by optional
adapter crates and features. The workspace includes the experimental
`zstd-zero` companion decoder for allocation-free layer decompression.

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

## Examples

Download the index, platform manifests, and config blobs for a public Datadog
Agent package without downloading its package layers:

```console
cargo run --example download_metadata
```

The example defaults to
`oci://install.datadoghq.com/agent-package@sha256:7ab3a71476f068c21399250e66a2b1ab366437489510ee12c2119bba75afcde9`.
Pass a different public `oci://` reference as its first argument to inspect it
instead. The example is a host-side prototype and does not form part of the
allocation-free core API.

Stream and decode a current Datadog `tar+zstd` config layer, verifying both its
compressed and decompressed SHA-256 digests without buffering either stream:

```console
cargo run --release -p zstd-zero --example decode_layer
```

The experimental decoder lives in the unpublished `zstd-zero` workspace crate.
It is `no_std`, dependency-free, and allocation-free; this host-side smoke test
uses heap-backed caller buffers for its 32 MiB history window and two 128 KiB
scratch areas.

Extract one regular file from that layer while it is downloaded and decoded:

```console
cargo run --release --example extract_file -- \
  application_monitoring.yaml.example > application_monitoring.yaml.example
```

The tar state machine keeps one 512-byte header and writes matching contents
directly to standard output. Layer bytes are never buffered in full. The fixed
32 MiB allocation is required by this particular Zstandard frame's declared
history window; the decoder also reuses two 128 KiB work areas and a 16 KiB
network input buffer. The host-side HTTP adapter may perform its own small
allocations. Because standard output is not transactional, consumers must wait
for a successful process exit before trusting or acting on the streamed bytes.

For a hosted `no_std` binary with no Rust allocator, build the `rustix` adapter
and give it an HTTPS URL, a local path, or `-` for standard input:

```console
cargo build --release -p oci-zero-no-std-extract
target/release/oci-zero-no-std-extract \
  'https://install.datadoghq.com/v2/agent-package/blobs/sha256:bc219703080f03ad836d51bf7f72cc3ced34f5ba440a49f450d6a5dea98ceff4' \
  application_monitoring.yaml.example > application_monitoring.yaml.example
```

The same binary can read an already downloaded blob with
`oci-zero-no-std-extract datadog-config-layer.tar.zst ENTRY`, or read the blob
from standard input when the source argument is `-`.

The network path composes reqwless's streaming request/response API with
`mbedtls-rs`; it does not collect the response body. MbedTLS's internal C
allocations come from a fixed 4 MiB static bump arena, while the Rust program
has no global allocator. The Zstandard decoder's 32 MiB history, two 128 KiB
work areas, 16 KiB input area, and the HTTP/TLS buffers are also bounded and
statically or stack allocated. This proof-of-concept is deliberately narrow:
it verifies the known Datadog layer's compressed and decompressed sizes and
digests, trusts only the embedded DigiCert Global Root G2, resolves DNS through
`8.8.8.8`, and creates one TLS session per process. The hosted adapter requires
Rust 1.91; the allocation-free core crate remains compatible with Rust 1.75.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE)), or
- MIT License ([LICENSE-MIT](LICENSE-MIT))

at your option.
