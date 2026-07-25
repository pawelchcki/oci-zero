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
export OCI_ZERO_ROOT=$(cd ../.. && pwd)
cross build --release --target riscv32imc-unknown-none-elf --features full
```

`cross` rather than `cargo`, because mbedTLS is compiled from source for
riscv32 and needs a C compiler with a RISC-V backend, which macOS does not
have. Without the `tls` feature a plain `cargo build` works fine.

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
| `tls`      | mbedTLS via `oci_zero::tls::connect` (implies `radio`)        |
| `matter`   | `rs-matter-embassy`'s `EmbassyWifiMatterStack`                |
| `full`     | everything — the rung the decision rests on                   |

The layers are *referenced*, not merely depended on. With `lto = "fat"` a
dependency nothing calls is discarded entirely, so a rung that only adds a
`Cargo.toml` line measures nothing — which is exactly what the first two attempts
at this measurement did. Every `reference_*` function in `src/main.rs` exists to
drag real code paths into the linked image; see the comments there for how each
one defeats dead-code elimination, and why the mbedTLS rung has to *poll* a
future rather than merely construct one.

## Pinned dependencies

- `rs-matter-embassy` is pinned by git revision: its crates.io entry is a
  `0.0.0` placeholder, and it tracks esp-hal's git tree rather than its releases.
- The esp-hal family is pinned to the same revision `rs-matter-embassy`'s own
  `examples/esp` uses, because that is what its API expects.
- `mbedtls-rs-sys` is pinned to esp-rs's fork. crates.io's version ships
  *prebuilt* riscv32imc libraries with no esp-hal hooks; the fork builds from
  source, so the mbedTLS config is ours and the hardware SHA, bignum and time
  hooks exist.
- Matter uses `rs-matter-embassy`'s default `rustcrypto` backend rather than
  mbedTLS. Sharing one crypto stack would save flash, and flash is the resource
  there is most of.

## Still to do

- Matter commissioning over BLE, with the QR code committed to this README
  (plan Chunk 5).
- The OCI self-update path: `Fetcher`/`PullVisitor` over mbedTLS + reqwless,
  streaming the verified layer into the inactive OTA slot (plan Chunk 6).
- Stack high-water during a commissioning attempt. This needs the board and is
  the one number MEASUREMENTS.md cannot supply.
