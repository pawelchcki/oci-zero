#!/bin/sh
# Measures flash and RAM for each rung of the feature ladder.
#
# Answers the question Chunk 4 of the plan exists to answer: does esp-hal +
# esp-radio + rs-matter-embassy + oci-zero-with-TLS fit on an ESP32-C3? Building
# each rung on its own attributes the cost to the layer that spends it instead of
# producing one number that cannot be acted on.
#
# Writes MEASUREMENTS.md-shaped output to stdout. Requires espflash (already
# needed as the cargo runner) and python3.
set -eu

cd "$(dirname "$0")"

TARGET=riscv32imc-unknown-none-elf
ELF=target/$TARGET/release/oci-zero-esp32c3-ota
IMAGE=target/measure-image.bin

# ESP32-C3 has 400 KB of SRAM. The ROM bootloader and the wireless stacks claim
# part of it, and ~320 KB is the usual usable figure for an application.
USABLE_DRAM=327680
# Two OTA slots alongside nvs/otadata/phy_init in 4 MB of flash.
OTA_SLOT=1835008
# Must track HEAP_SIZE in src/main.rs.
HEAP=102400

printf '%-10s%9s%8s%9s%9s%9s%9s%8s%9s%8s\n' \
    rung image 'of ota' .text .rodata static +heap 'of ram' headroom build

for features in "" oci radio tls matter full; do
    label=${features:-baseline}
    start=$(date +%s)

    if [ -z "$features" ]; then
        set -- --release
    else
        set -- --release --features "$features"
    fi
    if ! cargo build "$@" >/dev/null 2>&1; then
        printf '%-10s%s\n' "$label" "  BUILD FAILED — rerun without the redirect to see why"
        continue
    fi
    elapsed=$(( $(date +%s) - start ))

    # The padded, page-aligned image is what occupies the OTA slot, not the sum
    # of the ELF's sections.
    espflash save-image --chip esp32c3 "$ELF" "$IMAGE" >/dev/null 2>&1
    image=$(wc -c < "$IMAGE" | tr -d ' ')

    python3 elfsize.py "$ELF" "$label" "$elapsed" "$image" "$HEAP" "$USABLE_DRAM" "$OTA_SLOT"
done

cat <<EOF

Budgets: OTA slot $OTA_SLOT bytes; usable DRAM $USABLE_DRAM bytes; heap $HEAP bytes.
"static" is .data + .bss with linker padding, the leftover .stack region and the
reclaimed DRAM2 heap region excluded. "headroom" is the .stack section, which the
linker sizes from whatever DRAM is left after every static — so it is the number
that must stay comfortably above the deepest call chain, not a reservation.
EOF
