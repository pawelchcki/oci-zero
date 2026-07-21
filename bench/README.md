# End-to-end overhead harness

This harness measures the hosted `no_std` extractor while it downloads and
processes the repository's pinned Datadog OCI layer. The process performs the
HTTPS request, TLS, compressed and decompressed SHA-256 verification,
Zstandard decoding, tar scanning, and extraction. Output is sent to
`/dev/null` but is still produced by the measured process.

The fixture transfers 3,651,442 bytes and expands to 10,878,976 bytes (10.38
MiB). Keeping the digest-pinned fixture already used by CI makes runs
comparable and prevents a mutable registry tag from silently changing the
workload. The transferred and decoded sizes are both reported so results are
not accidentally described as a 10 MiB network transfer.

## Run it

The profilers require Linux, GNU `time`, Valgrind, Python 3, and Rust 1.91 or
newer:

```console
bench/e2e-overhead.sh
```

Select one of the large small-window fixtures with:

```console
bench/e2e-overhead.sh --list-fixtures
bench/e2e-overhead.sh --fixture ghcr-64m-w64k
bench/e2e-overhead.sh --fixture ghcr-256m-w256k
```

The default instruction counter is Callgrind's deterministic `Ir` event. To
measure hardware-retired user-space instructions on a stable, otherwise idle
Linux host that permits performance counters, use:

```console
bench/e2e-overhead.sh --instruction-counter perf
```

On macOS, run the manually dispatched **E2E overhead** GitHub Actions workflow.
It uploads the JSON result and raw profiler outputs as an artifact. Docker is
also supported for a local Callgrind run:

```console
bench/e2e-overhead-docker.sh
```

The container uses a separate `target/e2e-linux` build directory so it does
not mix Linux and macOS artifacts.

Results default to `target/e2e-overhead/FIXTURE/results.json`. Each profiler
performs a fresh live download. Run on the same machine class and instruction
counter when comparing commits; wall time remains sensitive to the network and
CDN.

## What is measured

- `peak_native_heap_bytes` is zero: the binary has no Rust global allocator,
  and its C `calloc`/`free` symbols use the fixed static TLS arena instead of
  the operating-system heap.
- `massif_peak_live_allocation_bytes` is the peak concurrent allocation demand
  including bookkeeping; the payload-only value is also reported. Massif
  interposes on `calloc`, so this says how much live heap a conventional
  allocator would need rather than claiming the native binary uses that heap.
- `peak_rss_bytes` includes mapped code, touched static buffers, stack, TLS
  arena, and heap.
- `tls_arena_peak_bytes` is the high-water mark inside MbedTLS's fixed 4 MiB
  static arena. It is not heap, so Massif cannot represent it.
- `decoder_static_buffer_bytes` is the fixed Zstandard history and scratch
  storage. It is also not heap.
- `instructions` is user-space Callgrind `Ir` by default, or native retired
  user-space instructions with `perf`. Kernel and network-device work is not
  included in either counter.
- `peak_stack_bytes`, elapsed and CPU time, release binary size, and normalized
  per-byte values are included for context.

Massif, RSS, and instruction counts come from separate process executions so
the profilers do not perturb one another.

## Large small-window fixtures

`generate-small-window-fixtures.py` creates deterministic OCI image layouts
containing effectively incompressible tar+zstd layers. The default fixtures
are 64 MiB with a 64 KiB declared history window and 256 MiB with a 256 KiB
window. Their wire sizes stay close to their decoded sizes, making them useful
for measuring real large downloads instead of highly compressed toy payloads:

```console
bench/generate-small-window-fixtures.py
```

After authenticating `crane` to GHCR with a classic token carrying
`write:packages`, publish the immutable `v1-*` tags with:

```console
bench/publish-small-window-fixtures.sh
```

The generated OCI config links the package to this public repository using
`org.opencontainers.image.source`. The publish script refuses to replace a tag
when GHCR already contains a different manifest digest.

The published immutable manifests are:

- `ghcr.io/pawelchcki/oci-zero-small-window-fixtures@sha256:586c2d89a1b1464f2d383cffd414eb81dea5f717881775f46fbab45148b2870c`
  (64 MiB payload, 64 KiB window).
- `ghcr.io/pawelchcki/oci-zero-small-window-fixtures@sha256:e92048e37a5696ec6f9208f1fd62e3b5ee5672544a1d08954b9716e004c773c1`
  (256 MiB payload, 256 KiB window).

The hosted `no_std` extractor accepts the fixture's compressed and
decompressed digest, sizes, and history limit as optional positional arguments.
This lets the same allocation-free binary validate all fixtures while handing
the decoder only the history slice declared by that fixture.
