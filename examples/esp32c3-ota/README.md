# ESP32-C3 OCI self-update

Firmware for an ESP32-C3 that pulls its own next version from an OCI registry.

**Status: feasibility measurement only.** The binary links every layer the
finished firmware will contain and does nothing with them. Read
[MEASUREMENTS.md](MEASUREMENTS.md) first — it answers whether this fits on the
part, and it corrects several assumptions that turned out to be wrong. Matter
commissioning and the OCI update path are not implemented yet.

## Building

A separate workspace with its own target, toolchain and `[patch.crates-io]`
pins, excluded from the repository root's workspace *and* package so
`cargo build --workspace` and `cargo package -p oci-zero` are unaffected.

```console
cargo build --release --features full
```

Plain `cargo` on any host: nothing here compiles C. `.cargo/config.toml` sets the
target, so `--target` is optional.

To take the measurements:

```console
./measure.sh
```

## Feature ladder

Each rung adds one layer, so `measure.sh` can attribute flash and RAM to the
layer that spends it rather than reporting one unactionable number.

| feature    | adds                                                        |
| ---------- | ----------------------------------------------------------- |
| *(none)*   | esp-hal, esp-rtos, esp-alloc, the heap                      |
| `oci`      | `oci-zero`'s `pull()` walk and gzip decoder                  |
| `radio`    | esp-radio Wifi **and** BLE controllers                       |
| `tls`      | a verified `embedded-tls` 1.3 handshake                       |
| `matter`   | `rs-matter-embassy`'s `EmbassyWifiMatterStack`                |
| `full`     | everything — the rung the decision rests on                   |

The layers are *referenced*, not merely depended on. With `lto = "fat"` a
dependency nothing calls is discarded entirely, so a rung that only adds a
`Cargo.toml` line measures nothing — which is exactly what the first two attempts
at this measurement did. Every `reference_*` function in `src/main.rs` exists to
drag real code paths into the linked image; see the comments there for how each
one defeats dead-code elimination.

## Pinned dependencies

- `rs-matter-embassy` is pinned by git revision: its crates.io entry is a
  `0.0.0` placeholder, and it tracks esp-hal's git tree rather than its releases.
- The esp-hal family is pinned to the same revision `rs-matter-embassy`'s own
  `examples/esp` uses, because that is what its API expects.
- TLS is `embedded-tls`, not `oci-zero`'s mbedTLS connector, and Matter uses
  `rs-matter-embassy`'s default `rustcrypto` backend. The firmware is therefore
  pure Rust apart from esp-radio's binary PHY blobs. See MEASUREMENTS.md for what
  that costs and what it still leaves unsettled — notably that certificate
  validity dates are unchecked until the device has a clock.
- `spin` is depended on directly only to enable its `portable_atomic` feature:
  riscv32imc has no atomic compare-exchange, and `rsa` reaches `spin` through
  num-bigint-dig.

## Still to do

- Matter commissioning over BLE, with the QR code committed to this README
  (plan Chunk 5).
- The OCI self-update path: `Fetcher`/`PullVisitor` over mbedTLS + reqwless,
  streaming the verified layer into the inactive OTA slot (plan Chunk 6).
- Stack high-water during a commissioning attempt. This needs the board and is
  the one number MEASUREMENTS.md cannot supply.
