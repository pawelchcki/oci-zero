"""Splits an espflash `--merge` image into the three pieces a flasher writes.

`espflash save-image` emits either the application alone or one merged image
covering the whole flash. Neither is what the OCI artifact wants: shipping only
the application cannot provision a board whose partition table puts the app
somewhere else — which is exactly how the first published artifact failed to boot,
because it wrote an app at 0x20000 onto a board whose default table expected one at
0x10000 — and shipping a 4 MB merged blob wastes most of the layer on padding.

So the merged image is cut back into bootloader, partition table and application,
each at its own offset, and the artifact carries all three. A browser can then
provision a blank board, while the device's own OTA path picks `firmware.bin` out
of the layer by name and ignores the rest.

    python3 tools/split-flash-image.py merged.bin out/

Offsets are read from the merged image's own layout rather than assumed: the
partition table's location is a build-time choice and 0x8000 is only the default.
"""

import pathlib
import sys

# Where the second-stage bootloader starts on every ESP32 part.
BOOTLOADER_OFFSET = 0x0
# ESP-IDF's partition-table magic, little-endian 0x50AA.
TABLE_MAGIC = b"\xaa\x50"
# A partition table is at most 0xC00 bytes and is padded to a 0x1000 boundary.
TABLE_MAX_LEN = 0xC00


def trim(block: bytes) -> bytes:
    """Drops trailing erase-state bytes.

    Flash reads as 0xFF when erased, and espflash pads each region out to its
    slot. Writing that padding would be harmless but would bloat the layer, and
    for the bootloader it would overwrite the partition table.
    """
    return block.rstrip(b"\xff")


def find_table_offset(image: bytes, candidates=(0x8000, 0x9000, 0x10000)) -> int:
    for offset in candidates:
        if image[offset : offset + 2] == TABLE_MAGIC:
            return offset
    raise SystemExit(
        "no partition table found at any of "
        + ", ".join(hex(candidate) for candidate in candidates)
    )


def find_app_offset(image: bytes, table_offset: int) -> int:
    """Reads the first app partition's offset straight out of the table.

    Each entry is 32 bytes: magic(2) type(1) subtype(1) offset(4) size(4)
    label(16) flags(4) — so `offset` is at 4..8 and `size` at 8..12. Type 0 is an
    app partition.
    """
    position = table_offset
    while position + 32 <= len(image):
        entry = image[position : position + 32]
        if entry[:2] != TABLE_MAGIC:
            break
        kind = entry[2]
        offset = int.from_bytes(entry[4:8], "little")
        if kind == 0:
            return offset
        position += 32
    raise SystemExit("the partition table lists no app partition")


def main() -> None:
    if len(sys.argv) != 3:
        raise SystemExit(f"usage: {sys.argv[0]} MERGED_IMAGE OUTPUT_DIR")
    merged = pathlib.Path(sys.argv[1]).read_bytes()
    out = pathlib.Path(sys.argv[2])
    out.mkdir(parents=True, exist_ok=True)

    table_offset = find_table_offset(merged)
    app_offset = find_app_offset(merged, table_offset)
    if not (BOOTLOADER_OFFSET < table_offset < app_offset):
        raise SystemExit(
            f"unexpected layout: table {table_offset:#x}, app {app_offset:#x}"
        )
    if merged[app_offset] != 0xE9:
        raise SystemExit(f"no application image magic at {app_offset:#x}")

    pieces = [
        ("bootloader.bin", BOOTLOADER_OFFSET, trim(merged[BOOTLOADER_OFFSET:table_offset])),
        (
            "partition-table.bin",
            table_offset,
            trim(merged[table_offset : table_offset + TABLE_MAX_LEN]),
        ),
        ("firmware.bin", app_offset, trim(merged[app_offset:])),
    ]

    for name, offset, contents in pieces:
        if not contents:
            raise SystemExit(f"{name} came out empty at {offset:#x}")
        (out / name).write_bytes(contents)
        print(f"{name} {offset:#x} {len(contents)} bytes")

    # Emitted so the caller can build the --image arguments without repeating the
    # offsets, which is the thing that went wrong before.
    print(
        "IMAGE_ARGS="
        + " ".join(f"--image {out / name}:{offset:#x}" for name, offset, _ in pieces)
    )


if __name__ == "__main__":
    main()
