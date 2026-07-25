# Does it fit on an ESP32-C3?

Yes, with room to spare in flash and roughly a quarter of DRAM uncommitted.

Reproduce with `./measure.sh` — plain `cargo`, no container and no C toolchain.
Numbers below are from `rustc 1.95.0`, `opt-level = "z"`, `lto = "fat"`,
`codegen-units = 1`, `panic = "abort"`.

| rung        | image (OTA) |    of slot |     .text | .rodata | static RAM |  + heap |   of DRAM | stack headroom |
| ----------- | ----------: | ---------: | --------: | ------: | ---------: | ------: | --------: | -------------: |
| `baseline`  |      93,888 |       4.8% |    32,764 |  12,480 |     38,164 | 140,564 |     42.9% |        277,860 |
| `oci`       |     105,664 |       5.4% |    44,410 |  15,404 |     43,468 | 145,868 |     44.5% |        272,684 |
| `radio`     |     525,984 |      26.8% |   444,646 |  73,916 |     57,580 | 159,980 |     48.8% |        204,528 |
| `tls`       |     344,448 |      17.5% |   283,190 |  34,688 |     69,436 | 171,836 |     52.4% |        246,716 |
| `matter`    |     529,520 |      26.9% |   446,110 |  75,956 |    135,820 | 238,220 |     72.7% |        126,288 |
| `full`      |     801,104 |      40.7% |   695,220 |  97,244 |    167,108 | 269,508 |     82.2% |         95,128 |
| **`device`** | **1,253,200** | **63.7%** | 1,046,268 | 188,920 |    150,388 | 252,788 | **77.1%** |    **111,656** |

`device` is the real firmware — `full` without `measure`, so it runs the Matter
stack rather than the reference functions. **It is 450 KB larger than `full`,**
because allocating the Matter stack links almost none of its runtime: the data
model, the interaction model, commissioning and mDNS only appear once something
calls `run_coex`. The rungs are for attributing cost between layers; `device` is
the number that has to fit.

Its static RAM is *lower* than `full`'s because the TLS record buffers are not
there yet — nothing calls TLS until the OCI update path lands. Expect roughly
+240 KB of flash and +26 KB of static RAM when it does, which lands near 1.5 MB
(76% of a slot) and 176 KB of statics.

Budgets: one OTA slot of 1,835,008 bytes (1.75 MB, two slots plus
`nvs`/`otadata`/`phy_init` in 4 MB); 327,680 bytes of usable DRAM; a 100 KB heap.

`full` is the rung that decides: esp-hal + esp-radio (Wifi **and** BLE) +
`rs-matter-embassy` + `oci-zero` + a verified `embedded-tls` handshake.

## What each layer costs

Static RAM is very nearly additive — the five rows below sum to 167,092 against
`full`'s 167,108 — so each is the layer's own cost. Flash is not quite, because
LTO shares code between layers; the rows sum about 21 KB below `full`.

| layer                          |        flash |  static RAM |
| ------------------------------ | -----------: | ----------: |
| esp-hal + esp-rtos + heap      |       93,872 |      38,164 |
| `oci-zero` (pull + gzip)       | **+11,760** | **+5,304** |
| `embedded-tls` (TLS 1.3 + PKI) |    +238,784 |     +25,968 |
| esp-radio (Wifi + BLE)         |    +432,080 |     +19,416 |
| Matter stack                   |      +3,552 | **+78,240** |

Two things stand out, and both are the opposite of what the plan assumed:

- **`oci-zero` is nearly free.** 11.8 KB of flash and 5.3 KB of RAM, which is
  what an allocation-free streaming design buys. It is the cheapest layer here by
  more than an order of magnitude.
- **Flash was never the risk.** The plan worried about the 1.75 MB OTA slot;
  the answer uses well under half of one. Radio and TLS dominate, and neither is
  negotiable.

The real constraint is **static RAM, and most of it is Matter's**: 78 KB of the
167 KB of statics is the Matter stack, matching `rs-matter-embassy`'s own "~35 to
50KB" estimate plus the 20 KB bump allocator and the embassy task arena.
`embedded-tls`'s 26 KB is almost entirely its two record buffers.

## Why embedded-tls and not mbedTLS

`oci-zero`'s `tls` feature is an mbedTLS connector, and mbedTLS is C. Measured
both ways, on the `full` rung:

| |          flash | static RAM | stack headroom | build needs |
| --- | ---: | ---: | ---: | --- |
| mbedTLS      | 909,296 |    144,892 |        117,344 | cmake, a RISC-V-capable clang, a container on macOS |
| embedded-tls | **801,072** | 167,108 |     95,128 | `cargo build` |

embedded-tls costs 108 KB less flash and 22 KB more static RAM — the RAM being
its record buffers, which mbedTLS kept on the heap instead, so the difference is
partly bookkeeping. What it really buys is that **no C is compiled at all**: no
cmake, no `riscv32-esp-elf-gcc`, no clang-with-a-RISC-V-backend, no container,
and no libc for mbedTLS's `printf`/`puts`/`snprintf`/`vsnprintf`. `cargo build`
works on any host.

`oci-zero` needed no change. Its `reqwless` adapter is transport-agnostic —
`send_on` takes any `Read + Write` — so an `embedded_tls::TlsConnection` plugs
straight in, and the mbedTLS-specific `tls` feature simply goes unused here.

### What still has to be settled

- **Certificate validity dates are not checked.** The verifier is instantiated
  with `NoClock`, which skips them. A real clock is needed, and the device has no
  RTC across reboots — Matter commissioning or an SNTP query has to supply one.
- **The trust anchor is a placeholder.** `CA_CERTIFICATE` is zeroed bytes sized
  like a real root, enough to measure the verifier's code and buffers. Chunk 6
  has to pin the actual roots: `ghcr.io` is Sectigo-issued and the blob redirect
  lands on `pkg-containers.githubusercontent.com`, which is Let's Encrypt.
- **RSA is mandatory, and its speed on this core is unmeasured.** Both hosts
  serve RSA certificates signed `sha256WithRSAEncryption`, so an ECDSA-only
  verifier cannot validate either chain. Pure-Rust RSA verification on a 160 MHz
  RISC-V core with no hardware bignum accelerator may take appreciable time; this
  is the main thing hardware testing has to confirm.
- **TLS 1.3 only.** Both hosts support it.

## `spin` and the missing A extension

`rsa` reaches `spin` through num-bigint-dig and lazy_static, and spin's default
uses `core::sync::atomic` compare-exchange — which riscv32imc does not have,
the chip having no A extension. spin's `portable_atomic` feature routes those
through `portable-atomic` instead, which esp-hal already configures for a single
core, so `spin` is depended on directly for the sole purpose of turning that
feature on.

## What is left for Chunk 6

95 KB of stack headroom and 100 KB of heap, against `PullBuffers` — 2 KB root
manifest + 2 KB child manifest + 512 B config in the measurement, generous for an
artifact whose manifest is ~630 bytes — and the embassy-net socket buffers the TLS
stream will sit on. The gzip history window, 32 KB, is already counted.

**`headroom` is not a reservation.** esp-hal's linker script gives `.stack`
whatever DRAM is left after every static, so it shrinks as statics grow. It is
the number that has to stay above the deepest call chain — and the deepest call
chain is a Matter commissioning exchange happening while a TLS handshake runs,
which **only hardware can measure**. That measurement is outstanding and is the
one number in the plan's Chunk 4 list that this report cannot supply.

## Corrections to the plan's assumptions

1. **"TLS already cross-compiles for the target… clang cross-compiles
   `mbedtls-rs-sys`'s C with no `riscv32-esp-elf-gcc` needed" — wrong.** No C was
   compiled at all: crates.io's `mbedtls-rs-sys` ships *prebuilt* `.a` files for
   `riscv32imc-unknown-none-elf`, which is why that build took 7 seconds, and they
   carry no esp-hal hooks. Building the esp-aware fork from source needs a
   RISC-V-capable C compiler, which macOS does not have. Moot now: there is no C.
2. **`rs-matter-embassy` is not published.** Its crates.io entry is a `0.0.0`
   placeholder; it is pinned here by git revision, as are the esp-hal crates,
   because it tracks esp-hal's git tree rather than its releases.
3. **Nothing needs mbedTLS.** `rs-matter-embassy` defaults to `rustcrypto`, and
   the OCI path uses `embedded-tls`, so the whole firmware is pure Rust apart from
   esp-radio's binary PHY blobs.
4. **The zstd-versus-gzip worry was unnecessary.** gzip stays because it is
   smaller and nothing needs zstd, not because RAM forces it.
5. **`esp-bootloader-esp-idf` is not ESP-IDF C.** It is a pure-Rust
   implementation of the IDF bootloader's partition table and app-descriptor
   formats. It stays, because the OTA slot switching and the `esp_app_desc!()`
   version the flasher page reads back both depend on those formats, and the
   second-stage bootloader in flash is Espressif's either way.
