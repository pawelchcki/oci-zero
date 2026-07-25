# Does it fit on an ESP32-C3?

Yes, with room to spare in flash and roughly a quarter of DRAM uncommitted.

Reproduce with `./measure.sh`. Numbers below are from
`rustc 1.95.0`, `opt-level = "z"`, `lto = "fat"`, `codegen-units = 1`,
`panic = "abort"`, built through `cross` (see [Why cross](#why-cross)).

| rung       | image (OTA) |    of slot |   .text | .rodata | static RAM |  + heap |   of DRAM | stack headroom |
| ---------- | ----------: | ---------: | ------: | ------: | ---------: | ------: | --------: | -------------: |
| `baseline` |      93,872 |       5.1% |  32,746 |  12,480 |     38,164 | 140,564 |     42.9% |        277,860 |
| `oci`      |     105,664 |       5.8% |  44,416 |  15,408 |     43,464 | 145,864 |     44.5% |        272,688 |
| `radio`    |     525,872 |      28.7% | 444,538 |  73,924 |     57,580 | 159,980 |     48.8% |        204,528 |
| `tls`      |     905,872 |      49.4% | 721,042 | 175,692 |     66,644 | 169,044 |     51.6% |        195,592 |
| `matter`   |     529,472 |      28.9% | 446,062 |  75,956 |    135,820 | 238,220 |     72.7% |        126,288 |
| **`full`** | **909,296** |  **49.6%** | 722,384 | 177,732 |    144,892 | 247,292 | **75.5%** |    **117,344** |

Budgets: one OTA slot of 1,835,008 bytes (1.75 MB, two slots plus
`nvs`/`otadata`/`phy_init` in 4 MB); 327,680 bytes of usable DRAM; a 100 KB heap.

`full` is the rung that decides: esp-hal + esp-radio (Wifi **and** BLE) +
`rs-matter-embassy` + `oci-zero` with `tls` and `gzip`, with mbedTLS's handshake
genuinely linked (770 mbedTLS symbols in the image).

## What each layer costs

| layer                          |        flash |  static RAM |
| ------------------------------ | -----------: | ----------: |
| esp-hal + esp-rtos + heap      |       93,872 |      38,164 |
| `oci-zero` (pull + gzip)       | **+11,792** | **+5,300** |
| esp-radio (Wifi + BLE)         |    +420,208 |     +14,116 |
| mbedTLS (TLS 1.2/1.3 + X.509)  | **+380,000** |     +9,064 |
| Matter stack                   |      +3,424 | **+78,248** |

Each row is the cost over the rung below it. `matter` in the table above is
measured without TLS, which is why it looks cheap in flash there; the Matter row
here is `full` minus `tls`.

Two things stand out, and both are the opposite of what the plan assumed:

- **`oci-zero` is nearly free.** 11.8 KB of flash and 5.3 KB of RAM, which is
  what an allocation-free streaming design buys. It is the cheapest layer here by
  more than an order of magnitude.
- **Flash was never the risk.** The plan worried about the 1.75 MB OTA slot;
  the answer uses half of one. Radio and mbedTLS dominate, and neither is
  negotiable.

The real constraint is **static RAM, and it is Matter's**: 78 KB of the 145 KB of
statics is the Matter stack, matching `rs-matter-embassy`'s own "~35 to 50KB"
estimate plus the 20 KB bump allocator and the embassy task arena.

## What is left for Chunk 6

117 KB of stack headroom and 100 KB of heap, against:

- mbedTLS record buffers, the largest remaining unknown. Defaults are 16 KB in
  and 16 KB out; `MBEDTLS_SSL_MAX_FRAGMENT_LENGTH` can cut that hard if needed.
- `PullBuffers`. The measurement uses 2 KB root manifest + 2 KB child manifest +
  512 B config, which is generous for an artifact whose manifest is ~630 bytes.
- The gzip history window, 32 KB, already counted in the `oci` rung.

So ~32 KB of TLS buffers plus ~5 KB of pull buffers against 117 KB of headroom.
That fits, and the fallback ladder in the plan (shrink record buffers, drop BLE
after commissioning, single OTA slot) is not needed.

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
   `riscv32imc-unknown-none-elf`, which is why that build took 7 seconds. Those
   prebuilt libraries also have no esp-hal hooks. Building the esp-aware fork from
   source does need a RISC-V-capable C compiler, and Apple's `/usr/bin/clang` has
   no RISC-V backend — hence `cross`.
2. **`rs-matter-embassy` is not published.** Its crates.io entry is a `0.0.0`
   placeholder; it is pinned here by git revision, as are the esp-hal crates,
   because it tracks esp-hal's git tree rather than its releases.
3. **Matter does not need mbedTLS.** `rs-matter-embassy`'s default `rustcrypto`
   backend works, so Matter and `oci-zero` do not have to share a crypto stack.
   Given that flash is abundant, that is the simpler choice and it is what these
   numbers reflect.
4. **The zstd-versus-gzip worry was unnecessary.** gzip remains the default, but
   the reason is no longer "the Matter budget cannot afford a zstd window" — with
   117 KB of headroom it probably could. gzip stays because it is smaller and
   nothing needs zstd.

## Why cross

mbedTLS is compiled from source for `riscv32imc-unknown-none-elf`. That needs a C
compiler with a RISC-V backend, and macOS's system clang has none:

    error: unable to create target: 'No available targets are compatible with
    triple "riscv32-unknown-unknown"'

A Linux clang has every LLVM backend, so the build happens in a container. See
`Cross.toml` and `Dockerfile.cross`. Two wrinkles worth knowing:

- `cross` does not read `[build] target` from `.cargo/config.toml`, so
  `--target riscv32imc-unknown-none-elf` has to be passed explicitly.
- `cross` only mounts the `oci-zero` path dependency when it is an *active*
  dependency, but cargo reads an optional path dependency's manifest either way.
  `Cross.toml` mounts it unconditionally via `OCI_ZERO_ROOT`, which `measure.sh`
  exports.

## Why `tls` implies `radio`

mbedTLS's C calls `printf`, `puts`, `snprintf` and `vsnprintf` from its
diagnostic paths, and `riscv32imc-unknown-none-elf` has no libc. Those four are
the only libc symbols missing, and esp-radio brings the ESP32-C3 ROM's libc,
which defines all of them — so the `tls` feature depends on `radio` and the link
succeeds. `tinyrlibc` is therefore only needed by the Matter half of the graph.

The dependency is also honest rather than a workaround: TLS with no network is
not a configuration this firmware ever wants.

The alternative was to stop `MBEDTLS_DEBUG_C` being defined at all, which would
mean giving `oci-zero` granular mbedTLS feature pass-through instead of
requesting `mbedtls-rs`'s `tls` bundle. That would shave some of the 380 KB, but
it changes the published crate's feature surface and `Tls::set_debug` would become
a no-op for every consumer, so it is deliberately not done here.
