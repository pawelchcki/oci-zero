# oci-zero

[![CI](https://github.com/pawelchcki/oci-zero/actions/workflows/ci.yml/badge.svg)](https://github.com/pawelchcki/oci-zero/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/oci-zero.svg)](https://crates.io/crates/oci-zero)
[![docs.rs](https://docs.rs/oci-zero/badge.svg)](https://docs.rs/oci-zero)

`oci-zero` aims to provide bounded-memory building blocks for accessing and
unpacking content from remote Open Container Initiative (OCI) registries.

> [!WARNING]
> **Experimental:** The allocation-free APIs are usable but remain subject to
> change while the registry and layer conformance suites grow.

```toml
[dependencies]
oci-zero = "0.1"
```

The default feature set is empty, so this pulls in only `sha2` and stays
`no_std`. Add the decoders and transport you need — see [Features](#features):

```toml
[dependencies]
oci-zero = { version = "0.1", features = ["gzip", "zstd"] }
```

## Capabilities

- Remain `no_std` and allocation-free by default.
- Parse references, indexes, manifests, image configs, descriptors, tags,
  annotations, and referrers as lazy borrowed views over caller buffers.
- Plan the complete read side of the OCI Distribution protocol, including
  authentication challenges, redirects, retries, pagination, and referrers
  fallback.
- Traverse selected platform manifests through a transport-independent fetcher
  and callback visitor without retaining descriptor collections.
- Verify descriptor sizes, compressed SHA-256 digests, and image layer diff IDs
  while content streams.
- Parse tar, ustar, per-entry PAX, and GNU long-path records into a
  transactional layer sink with safe paths and OCI whiteout events.
- Optionally decode gzip and Zstandard layers or use reqwless and MbedTLS,
  without adding a Rust allocator.
- Build the core for WebAssembly and use the included browser/Chrome extension
  example to browse catalogs, tags, metadata, layers, and merged filesystems.

## Features

The default feature set is empty:

- `gzip` integrates the separately published `gzip-zero` decoder.
- `zstd` integrates the separately published `zstd-zero` decoder.
- `reqwless` adds streaming HTTP over `embedded-io` connections.
- `tls` implies `reqwless` and adds the caller-configured MbedTLS connector.
- `docker-credentials` adds a host-side provider for Docker CLI
  `DOCKER_AUTH_CONFIG`, `credHelpers`, `credsStore`, and inline `auths`.

The core APIs use borrowed values, caller-owned buffers, visitors, and streaming
source/sink traits. They do not require a filesystem, executor, HTTP client, or
TLS implementation.

## Compatibility

The core, `gzip`, and `zstd` features support Rust 1.75 and targets without the
standard library. `docker-credentials` deliberately requires the standard
library so it can read local files and execute credential helpers. The current
reqwless adapter requires Rust 1.91; enabling `tls` retains that
feature-specific requirement.

## Examples

Download and verify a digest-pinned public OCI index, then print a short
summary:

```console
cargo run --features docker-credentials --example download_metadata
```

The example uses
`oci://install.datadoghq.com/agent-package@sha256:7ab3a71476f068c21399250e66a2b1ab366437489510ee12c2119bba75afcde9`.
Pass another reference to inspect a private registry using the credentials from
`$DOCKER_CONFIG/config.json` or `~/.docker/config.json`:

```console
docker login ghcr.io
cargo run --features docker-credentials --example download_metadata -- \
  oci://ghcr.io/OWNER/PRIVATE_IMAGE:TAG
```

No password or token is placed in the command line. The provider follows
Docker CLI precedence: a matching `DOCKER_AUTH_CONFIG` entry, then a
registry-specific `credHelpers` entry, then `credsStore`, and finally inline
`auths`. Helpers such as `docker-credential-desktop`,
`docker-credential-osxkeychain`, and registry-specific helpers are discovered
through `PATH` and invoked directly without a shell. Authentication errors do
not include credential values.

Run the detailed digest-pinned public registry fixture set used by CI:

```console
cargo test --release --test metadata_smoke \
  inspects_public_registry_fixtures -- --ignored --nocapture
```

The fixtures deliberately cover different public artifact repositories and
registry behavior:

- A multi-platform Datadog Agent package index on `install.datadoghq.com`, with
  custom `tar+zstd` layer media types and no authentication challenge.
- The Prometheus Helm chart on GHCR, including the Helm config, chart, and
  provenance media types behind anonymous Bearer authentication.
- The Bitnami Harbor Helm chart on Docker Hub, exercising a second Bearer token
  service and an annotated chart layer.
- A CRI-O bundle and its attached Sigstore and SPDX artifacts on GHCR,
  validating OCI 1.1 `artifactType` and `subject`. GHCR currently returns 404
  for the Referrers API on this repository, so the smoke test also exercises
  the distribution-spec referrers-tag fallback and tolerates legacy descriptor
  metadata after checking the authoritative manifests.
- The ORAS CLI's multi-platform OCI container image, including OCI image
  configs, layers, and attestation manifests.
- Kubernetes's `pause` image from `registry.k8s.io`, covering cross-registry
  redirects and Docker Schema 2 manifest-list, manifest, config, compressed
  layer, and Windows uncompressed-layer media types.

All fixture references use manifest digests rather than mutable tags. The
metadata smoke test intentionally skips artifact layers; the streamed layer
smoke test below covers that path.

Together these exercise the registry-facing parts of the OCI Image Spec, its
artifact guidance, OCI Distribution Spec 1.1, and Docker Schema 2. The OCI
Runtime Spec describes the runtime `config.json` and process lifecycle rather
than registry objects, so it is deliberately outside this metadata smoke test;
it needs a separate runtime adapter or container-runtime conformance test.

Stream and decode a current Datadog `tar+zstd` config layer, verifying both its
compressed and decompressed SHA-256 digests without buffering either stream:

```console
cargo run --release -p zstd-zero --example decode_layer
```

The decoder lives in the separately publishable `zstd-zero` workspace crate. It
is `no_std`, dependency-free, and allocation-free; this host-side smoke test
uses heap-backed caller buffers for its 32 MiB history window and two 128 KiB
scratch areas.

Extract one regular file from that layer while it is downloaded and decoded:

```console
cargo run --release --features zstd --example extract_file -- \
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

The network path uses `oci-zero`'s optional reqwless and MbedTLS adapters; it
does not collect the response body. MbedTLS's internal C
allocations come from a fixed 4 MiB static bump arena, while the Rust program
has no global allocator. The Zstandard decoder's 32 MiB history, two 128 KiB
work areas, 16 KiB input area, and the HTTP/TLS buffers are also bounded and
statically or stack allocated. This proof-of-concept is deliberately narrow:
it verifies the known Datadog layer's compressed and decompressed sizes and
digests, trusts only the embedded DigiCert Global Root G2, resolves DNS through
`8.8.8.8`, and creates one TLS session per process. The hosted adapter requires
Rust 1.91; the allocation-free core crate remains compatible with Rust 1.75.
CI builds and runs this binary against the pinned HTTPS blob, so the network,
certificate verification, fixed TLS arena, decoder, digest checks, tar parser,
and extraction path are exercised together.

## Browser and Chrome extension

A hosted build is published at
<https://pawelchcki.github.io/oci-zero/>. Because that is an ordinary HTTPS
origin, it can only reach registries that expose browser CORS headers; the
extension and CORS-disabled profile below cover the rest.

The [`web`](web/README.md) workspace crate builds `oci-zero`, gzip, and
Zstandard support for `wasm32-unknown-unknown`. Its shared `index.html` can be
served as an ordinary CORS-limited page or loaded as an unpacked Manifest V3
Chrome extension. Web mode talks to registries directly and can reach registries
that omit browser CORS headers when opened in a CORS-disabled Chrome profile,
while extension mode requests per-origin access directly from Chrome.

The example includes quick links for Docker Hub, GHCR, Quay, Kubernetes, ECR
Public, Microsoft Container Registry, `install.datadoghq.com`, and
`installtesting.datad0g.com`, plus the public repositories exercised by this
project. It lists paginated catalogs and tags, resolves platform manifests,
shows raw and structured metadata, applies verified layer whiteouts into a
merged filesystem, streams file listings through a background worker, extracts
individual files, and exports a selected platform as either an OCI image layout
or Docker-loadable archive.

```console
web/build.sh
cd web && npm run build:proxyless
```

See the web README for the CORS-disabled Chrome launch command, Chrome's
**Load unpacked** workflow, and the example's explicit browser memory limits.

## End-to-end overhead

The Linux profiling harness runs that same pinned download and records native
heap use, peak live allocation demand, peak RSS and stack, the fixed TLS arena
high-water, decoder static storage, CPU and wall time, binary size, and
user-space instructions. The 3.65 MB transfer decodes to a verified 10.88 MB
layer:

```console
bench/e2e-overhead.sh
bench/e2e-overhead.sh --fixture ghcr-64m-w64k
bench/e2e-overhead.sh --fixture ghcr-256m-w256k
```

See [bench/README.md](bench/README.md) for profiler requirements, metric
semantics, the optional hardware instruction counter, and the manual GitHub
Actions workflow available to macOS users.

## Contributing

See [AGENTS.md](AGENTS.md) for the PR title convention, which is enforced in CI
because it drives version bumps and changelogs, and for how releases are cut.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE)), or
- MIT License ([LICENSE-MIT](LICENSE-MIT))

at your option.
