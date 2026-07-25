"""Reports flash and RAM for one build of the firmware, for measure.sh.

Reads the ELF section header table directly, because a riscv32 binutils is not
otherwise needed to build this crate and requiring one only to measure would be a
poor trade.

Three of esp-hal's linker sections must not be counted as consumption:

  * `.rwdata_dummy` / `.rotext_dummy` are alignment padding, so the following
    section lands on an MMU page boundary. They occupy no real RAM.
  * `.stack` is not a reservation. The linker script hands it whatever DRAM is
    left after every static, so it is the *headroom* figure — it shrinks as
    statics grow, and counting it would make RAM always come out at ~100%.
  * `.dram2_uninit` is the region reclaimed from ROM code and handed to the heap,
    so it is part of the heap, not a static cost.

Flash is reported from the image espflash produces rather than from the ELF: the
OTA slot holds padded, page-aligned segments plus headers, and that padded size
is what has to fit.
"""

import struct
import sys

SHF_ALLOC = 0x2
SHF_WRITE = 0x1
SHF_EXECINSTR = 0x4
SHT_NOBITS = 8

PADDING = {".rwdata_dummy", ".rotext_dummy"}
HEADROOM = ".stack"
RECLAIMED = ".dram2_uninit"


def sections(path):
    with open(path, "rb") as handle:
        data = handle.read()
    if data[:4] != b"\x7fELF" or data[4] != 1 or data[5] != 1:
        raise SystemExit(f"{path} is not a little-endian ELF32 file")
    e_shoff, = struct.unpack_from("<I", data, 0x20)
    e_shentsize, e_shnum, e_shstrndx = struct.unpack_from("<HHH", data, 0x2E)

    def header(index):
        offset = e_shoff + index * e_shentsize
        return struct.unpack_from("<IIIIII", data, offset)

    _, _, _, _, str_off, str_size = header(e_shstrndx)
    strings = data[str_off:str_off + str_size]

    for index in range(e_shnum):
        name_off, kind, flags, addr, _, size = header(index)
        end = strings.index(b"\0", name_off)
        yield strings[name_off:end].decode(), kind, flags, addr, size


def measure(path):
    totals = {"text": 0, "rodata": 0, "data": 0, "bss": 0, "headroom": 0, "reclaimed": 0}
    for name, kind, flags, _addr, size in sections(path):
        if not flags & SHF_ALLOC or name in PADDING:
            continue
        if name == HEADROOM:
            totals["headroom"] += size
        elif name == RECLAIMED:
            totals["reclaimed"] += size
        elif kind == SHT_NOBITS:
            totals["bss"] += size
        elif flags & SHF_EXECINSTR:
            totals["text"] += size
        elif flags & SHF_WRITE:
            totals["data"] += size
        else:
            totals["rodata"] += size
    return totals


def main():
    path, label, elapsed, image_bytes, heap_bytes, usable_dram, ota_slot = sys.argv[1:8]
    elapsed, image_bytes, heap_bytes = int(elapsed), int(image_bytes), int(heap_bytes)
    usable_dram, ota_slot = int(usable_dram), int(ota_slot)

    totals = measure(path)
    static_ram = totals["data"] + totals["bss"]
    # The heap constant already includes the reclaimed region, which the linker
    # accounts for separately, so it is not added twice.
    committed = static_ram + heap_bytes

    print(
        f"{label:<10}"
        f"{image_bytes:>9}"
        f"{100 * image_bytes / ota_slot:>7.1f}%"
        f"{totals['text']:>9}"
        f"{totals['rodata']:>9}"
        f"{static_ram:>9}"
        f"{committed:>9}"
        f"{100 * committed / usable_dram:>7.1f}%"
        f"{totals['headroom']:>9}"
        f"{elapsed:>7}s"
    )


if __name__ == "__main__":
    main()
