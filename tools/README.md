# Firmware artifact tooling

## `build-firmware-artifact.sh`

Packages one or more ESP32 flash images as an OCI artifact, laid out as an
[OCI image layout](https://github.com/opencontainers/image-spec/blob/main/image-layout.md)
directory:

```console
tools/build-firmware-artifact.sh \
  --image firmware.bin:0x10000 \
  --version 0.1.0+sha.abc1234 \
  --output layout
```

The layout directory is the single source of truth. `crane push` reads a
directory as an OCI layout, and `tar` turns the same bytes into a release asset,
so the registry and the release cannot disagree:

```console
crane push layout ghcr.io/pawelchcki/oci-zero-firmware:latest
tar -cf firmware-0.1.0.oci.tar -C layout .
```

`--summary FILE` writes the resulting digests as `KEY=value` lines, which is how
`.github/workflows/firmware.yml` asserts that the digest GHCR reports equals the
manifest blob built locally.

### Schema

| Blob     | Media type                                                    |
| -------- | ------------------------------------------------------------- |
| manifest | `application/vnd.oci.image.manifest.v1+json`, `artifactType: application/vnd.oci-zero.firmware.v1+json` |
| config   | `application/vnd.oci-zero.firmware.config.v1+json`            |
| layer    | `application/vnd.oci-zero.firmware.layer.v1.tar+gzip`         |

Manifest annotations carry `org.opencontainers.image.version`,
`org.opencontainers.image.revision` and `vnd.oci-zero.firmware.chip`. The version
annotation is the single source for the flasher's display, `esp_app_desc!()` and
the Matter `SoftwareVersionString`, so all three agree by construction.

The config describes flash geometry so consumers need no per-chip knowledge:

```json
{ "chip": "esp32c3", "target": "riscv32imc-unknown-none-elf",
  "entries": [{ "path": "firmware.bin", "offset": 65536 }] }
```

### Two constraints that must survive edits

- **The layer media type has to end in `.tar+gzip` or `.tar+zstd`.**
  `browser_encoding` in `web/src/lib.rs` dispatches on that suffix, so a vendor
  media type is only decodable in the browser if it ends that way.
- **gzip is the default, not zstd.** The device cannot spare a Zstandard window
  next to the Matter stack. `--compression zstd` exists so that can be revisited
  if the memory measurements allow it.

The config uses a vendor media type and so carries no `rootfs.diff_ids`;
consumers verify the layer against its compressed descriptor digest and length
only, which is `VerifiedDecoder::compressed_only`.

### Reproducibility

The layer tar is written with pinned mode, uid/gid, names and `mtime=0`, and gzip
is asked for a zero mtime, so the same inputs give the same digest rather than
depending on whether GNU tar or bsdtar is installed. Requires `jq` and `python3`.

### Member names

`tar -cf out.tar -C layout .` names members `./index.json`; naming them
explicitly gives `index.json`. `oci-zero` matches the archive's own path, so
`web/flash.js` tries both spellings. On macOS, set `COPYFILE_DISABLE=1` to keep
bsdtar from adding AppleDouble `._*` members.
