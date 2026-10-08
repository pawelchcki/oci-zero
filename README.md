# oci-zero

[![CI](https://github.com/pawelchcki/oci-zero/actions/workflows/ci.yml/badge.svg)](https://github.com/pawelchcki/oci-zero/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/oci-zero.svg)](https://crates.io/crates/oci-zero)
[![docs.rs](https://docs.rs/oci-zero/badge.svg)](https://docs.rs/oci-zero)

Bounded-memory Rust building blocks for reading and unpacking content from
Open Container Initiative (OCI) registries. This repository publishes three
crates:

| Crate | Purpose | Docs |
| ----- | ------- | ---- |
| [`oci-zero`](https://crates.io/crates/oci-zero) | OCI registry client core: references, manifests, distribution requests, digest verification, tar layers | [docs.rs](https://docs.rs/oci-zero) |
| [`gzip-zero`](https://crates.io/crates/gzip-zero) | Streaming gzip decoder | [docs.rs](https://docs.rs/gzip-zero) |
| [`zstd-zero`](https://crates.io/crates/zstd-zero) | Streaming Zstandard decoder | [docs.rs](https://docs.rs/zstd-zero) |

All three are `no_std`, forbid `unsafe` code, and are allocation-free with
their default (empty) feature set. The decoders are published separately so
they can be used without `oci-zero`.

> [!WARNING]
> **Experimental:** The allocation-free APIs are usable but remain subject to
> change while the registry and layer conformance suites grow.

## `oci-zero`

```toml
[dependencies]
oci-zero = "0.2"
```

With default features this uses `sha2` for digests, `base64ct` for in-place
Base64, and `derive_more` for error formatting. Enable the decoders and
transport you need (the `gzip` and `zstd` features pull in `gzip-zero` and
`zstd-zero`, so you do not need to add them yourself):

```toml
[dependencies]
oci-zero = { version = "0.2", features = ["gzip", "zstd"] }
```

The core APIs use borrowed values, caller-owned buffers, visitors, and
streaming source/sink traits. They do not require a filesystem, executor, HTTP
client, or TLS implementation. They can:

- Serve a read-only OCI registry from borrowed memory through a transport-independent
  router, including catalog/tag pagination, manifests, blobs, and HEAD metadata.
- Parse references, indexes, manifests, image configs, descriptors, tags,
  annotations, and referrers as lazy borrowed views over caller buffers.
- Plan read-side OCI Distribution requests, including authentication
  challenges, redirects, retries, pagination, and the referrers-tag fallback.
- Traverse selected platform manifests through a transport-independent fetcher
  and callback visitor without retaining descriptor collections.
- Verify descriptor sizes, compressed SHA-256 digests, and image layer diff IDs
  while content streams.
- Parse tar, ustar, per-entry PAX, and GNU long-path records into a
  transactional layer sink with safe paths and OCI whiteout events.

### Features

| Feature | Adds |
| ------- | ---- |
| `gzip` | gzip layer decoding via `gzip-zero` |
| `zstd` | Zstandard layer decoding via `zstd-zero` |
| `reqwless` | Streaming HTTP over `embedded-io` connections |
| `tls` | Implies `reqwless`; adds a caller-configured MbedTLS connector |
| `docker-credentials` | Host-side (`std`) provider for Docker CLI `DOCKER_AUTH_CONFIG`, `credHelpers`, `credsStore`, and inline `auths` |

`gzip`, `zstd`, `reqwless`, and `tls` do not add a Rust allocator.

## `gzip-zero`

```toml
[dependencies]
gzip-zero = "0.1"
```

Wraps the allocation-free core of `miniz_oxide` with incremental gzip header,
trailer, checksum, size, and concatenated-member handling. The caller supplies
the 32 KiB DEFLATE history buffer; input may be split at arbitrary byte
boundaries and output is returned as borrowed slices. See
[gzip-zero/README.md](https://github.com/pawelchcki/oci-zero/blob/main/gzip-zero/README.md).

## `zstd-zero`

```toml
[dependencies]
zstd-zero = "0.2"
```

Decodes standard Zstandard frames with no dependencies. The caller supplies the
history buffer (at least the frame's declared window), two block scratch
buffers of up to 128 KiB, and the entropy-table buffers. Dictionary, legacy, and
magicless frames are not supported. Output can be observed before a final
content checksum fails, so callers that need atomic behaviour must use a
transactional sink. See [zstd-zero/README.md](https://github.com/pawelchcki/oci-zero/blob/main/zstd-zero/README.md) for the
buffer requirements and a usage example.

## Compatibility

- `oci-zero` (core, `gzip`, `zstd`), `gzip-zero`, and `zstd-zero` require Rust
  1.75 or newer and build for targets without the standard library. CI checks
  this on Rust 1.75 for `thumbv7em-none-eabi`.
- `docker-credentials` requires `std` so it can read local files and run
  credential helpers.
- The `reqwless` and `tls` features require Rust 1.91.

## Examples

[`examples/registry-demo`](examples/registry-demo/) runs an in-memory registry
locally or as a Cloudflare Worker, using the same `no_std` serving API. Explore
tiny layered images, a multi-platform index, and repository files packaged as
an OCI artifact with the web explorer's **Open local demo** button.

These examples run against digest-pinned public registry content.

Download and verify a public OCI index, then print a short summary:

```console
cargo run --features docker-credentials --example download_metadata
```

Pass another reference to use the credentials from
`$DOCKER_CONFIG/config.json` or `~/.docker/config.json`:

```console
docker login ghcr.io
cargo run --features docker-credentials --example download_metadata -- \
  oci://ghcr.io/OWNER/PRIVATE_IMAGE:TAG
```

No password or token is placed on the command line. The provider follows
Docker CLI precedence (`DOCKER_AUTH_CONFIG`, then `credHelpers`, then
`credsStore`, then inline `auths`), invokes helpers from `PATH` directly without
a shell, and does not include credential values in authentication errors.

Run the public registry fixture set used by CI (Docker Hub, GHCR,
`registry.k8s.io`, and `install.datadoghq.com`, covering Helm charts, OCI 1.1
artifacts and referrers, cross-registry redirects, and Docker Schema 2 media
types):

```console
cargo test --release --test metadata_smoke \
  inspects_public_registry_fixtures -- --ignored --nocapture
```

Stream and decode a Datadog `tar+zstd` layer, verifying its compressed and
decompressed SHA-256 digests without buffering either stream:

```console
cargo run --release -p zstd-zero --example decode_layer
```

Extract one file from that layer to standard output while it downloads:

```console
cargo run --release --features zstd --example extract_file -- \
  application_monitoring.yaml.example > application_monitoring.yaml.example
```

Standard output is not transactional, so wait for a successful process exit
before trusting or acting on the extracted bytes. This layer declares a 32 MiB
history window, which the example allocates on the heap; the host-side HTTP
adapter may also make small allocations.

[`no-std-extract`](https://github.com/pawelchcki/oci-zero/tree/main/no-std-extract) does the same as a hosted `no_std` binary
with no Rust allocator, using the `reqwless`, `tls`, and `zstd` features (Rust
1.91):

```console
cargo build --release -p oci-zero-no-std-extract
target/release/oci-zero-no-std-extract \
  'https://install.datadoghq.com/v2/agent-package/blobs/sha256:bc219703080f03ad836d51bf7f72cc3ced34f5ba440a49f450d6a5dea98ceff4' \
  application_monitoring.yaml.example > application_monitoring.yaml.example
```

It also accepts a local blob path, or `-` for standard input. It is a narrow
proof of concept: it checks the sizes and digests of that one known layer,
trusts only the embedded DigiCert Global Root G2, resolves DNS through
`8.8.8.8`, and gives MbedTLS a fixed 4 MiB static arena. CI builds and runs it
against the pinned blob.

## Other parts of this repository

These are not published crates:

- [`web`](https://github.com/pawelchcki/oci-zero/blob/main/web/README.md) — a WebAssembly browser page and Chrome extension for
  browsing registries, layers, and merged filesystems. A hosted build is at
  <https://pawelchcki.github.io/oci-zero/>; as an ordinary web page it can only
  reach registries that send CORS headers.
- [`bench`](https://github.com/pawelchcki/oci-zero/blob/main/bench/README.md) — a Linux harness that measures the end-to-end
  memory, CPU, and binary-size overhead of `no-std-extract`.
- [`examples/esp32c3-ota`](https://github.com/pawelchcki/oci-zero/blob/main/examples/esp32c3-ota/README.md) — ESP32-C3 firmware
  intended to update itself from an OCI registry; the update path is not
  implemented yet.

## Contributing

See [AGENTS.md](https://github.com/pawelchcki/oci-zero/blob/main/AGENTS.md) for the PR title convention, which is enforced in CI
because it drives version bumps and changelogs, and for how releases are cut.

[llm-cc complexity reports](.llm-cc/README.md) compare pull requests and rank
the repository on `main`; their scores are advisory.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](https://github.com/pawelchcki/oci-zero/blob/main/LICENSE-APACHE)), or
- MIT License ([LICENSE-MIT](https://github.com/pawelchcki/oci-zero/blob/main/LICENSE-MIT))

at your option.
