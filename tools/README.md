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

## `publish-static-registry.sh`

Rearranges an OCI image layout into the tree a registry's **read** path is made of, so a
bucket can serve `crane pull` with no server process:

```console
tools/publish-static-registry.sh --layout layout --name oci-zero/firmware \
  --tag latest --output publish
```

```
publish/
  v2/<name>/manifests/<digest>   the index, and every manifest it reaches
  v2/<name>/manifests/<tag>      a copy of the index bytes per tag
  v2/<name>/blobs/<digest>       every config and layer
  v2/<name>/tags/list
  uploads.tsv                    path, content type, cache-control
```

The distribution spec's read path is three GET shapes — `/v2/`,
`/v2/<name>/manifests/<ref>` and `/v2/<name>/blobs/<digest>` — so keys laid out to match
*are* a registry. The write path (`/v2/.../blobs/uploads/`) is not implemented and is not
meant to be: publishing is `aws s3 sync` of this directory.

### Why `uploads.tsv` exists

`aws s3 sync` sets one content type for a whole run, and a manifest served as
`application/octet-stream` is not a manifest as far as a client is concerned. Blobs are many
and uniformly opaque, so they go up in one sync; manifests are few and each carries its own
media type, so they are listed for the caller to `aws s3 cp` individually. The file also
carries `Cache-Control`: digests are immutable and cached for a year, tags for a minute.

`.github/workflows/registry.yml` does both passes and then pulls every object back with
`crane manifest` / `crane blob`, comparing byte for byte against the layout that produced it.

### Two things to know

- **Publishing replaces the repository.** The workflow syncs with `--delete` scoped to one
  `<name>`, so a run publishes exactly the tags passed to it and drops the rest. That is the
  only garbage collection a registry bucket can safely have: a blob is referenced by digest
  from any number of manifests, so nothing can expire one by age.
- **No `Docker-Content-Digest` header.** S3 serves a fixed set of response headers and that
  is not among them. `crane`, `skopeo` and `containerd` compute the digest from the body; a
  client that *requires* the header will not work against any static registry.

## `split-flash-image.py`

Cuts an `espflash save-image --merge` image back into `bootloader.bin`,
`partition-table.bin` and `firmware.bin`, each with its flash offset, and prints
an `IMAGE_ARGS=` line ready to pass to `build-firmware-artifact.sh`.

This exists because neither of espflash's outputs suits an OCI artifact. The
application alone cannot provision a board whose partition table puts the app
somewhere else — which is how the first published artifact failed to boot, writing
an app at `0x20000` onto a board whose default table expected one at `0x10000`,
so nothing ran and the device never advertised. The merged image would work, but
it is padded to the full 4 MB of flash and would spend the whole layer on
erase-state bytes.

Pass the *padded* merged image, not `--skip-padding`: the splitter treats file
offsets as flash offsets, and skipping padding compacts the file so they no longer
agree.
