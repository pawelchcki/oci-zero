#!/usr/bin/env python3

import argparse
import hashlib
import io
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tarfile


SOURCE = "https://github.com/pawelchcki/oci-zero"
PACKAGE = "ghcr.io/pawelchcki/oci-zero-small-window-fixtures"
ENTRY = "payload.bin"
MIB = 1024 * 1024
TAR_RECORD_SIZE = 20 * tarfile.BLOCKSIZE
DEFAULT_FIXTURES = (
    "v1-64m-w64k:64:16",
    "v1-256m-w256k:256:18",
)
ZSTD_VERSION = "v1.5.7"


class RepeatingPayload(io.RawIOBase):
    def __init__(self, size, motif):
        self.remaining = size
        self.motif = motif
        self.position = 0

    def readable(self):
        return True

    def readinto(self, buffer):
        length = min(len(buffer), self.remaining)
        written = 0
        while written < length:
            available = min(length - written, len(self.motif) - self.position)
            buffer[written : written + available] = self.motif[
                self.position : self.position + available
            ]
            written += available
            self.position = (self.position + available) % len(self.motif)
        self.remaining -= length
        return length


class HashingWriter:
    def __init__(self, destination):
        self.destination = destination
        self.hash = hashlib.sha256()
        self.size = 0

    def write(self, data):
        self.hash.update(data)
        self.size += len(data)
        return self.destination.write(data)

    def flush(self):
        self.destination.flush()


def canonical_json(value):
    return json.dumps(value, separators=(",", ":"), sort_keys=True).encode()


def digest_bytes(data):
    return f"sha256:{hashlib.sha256(data).hexdigest()}"


def digest_file(path):
    digest = hashlib.sha256()
    size = 0
    with path.open("rb") as source:
        while chunk := source.read(MIB):
            digest.update(chunk)
            size += len(chunk)
    return f"sha256:{digest.hexdigest()}", size


def tar_size(payload_size):
    unpadded = tarfile.BLOCKSIZE + payload_size + 2 * tarfile.BLOCKSIZE
    return (unpadded + TAR_RECORD_SIZE - 1) // TAR_RECORD_SIZE * TAR_RECORD_SIZE


def write_blob(layout, data):
    digest = digest_bytes(data)
    path = layout / "blobs" / "sha256" / digest.removeprefix("sha256:")
    path.write_bytes(data)
    return digest, len(data)


def link_blob(layout, source, digest):
    destination = layout / "blobs" / "sha256" / digest.removeprefix("sha256:")
    try:
        os.link(source, destination)
    except OSError:
        shutil.copyfile(source, destination)


def create_layout(output_dir, fixture, layer_path):
    name = fixture["name"]
    layout = output_dir / f"{name}.oci"
    if layout.exists():
        shutil.rmtree(layout)
    (layout / "blobs" / "sha256").mkdir(parents=True)
    (layout / "oci-layout").write_text(
        '{"imageLayoutVersion":"1.0.0"}\n', encoding="utf-8"
    )
    link_blob(layout, layer_path, fixture["compressed_digest"])

    labels = {
        "org.opencontainers.image.description": (
            f"oci-zero benchmark: {fixture['payload_bytes']} bytes with a "
            f"{fixture['window_bytes']}-byte Zstandard window"
        ),
        "org.opencontainers.image.licenses": "MIT OR Apache-2.0",
        "org.opencontainers.image.source": SOURCE,
        "io.oci-zero.fixture.entry": ENTRY,
        "io.oci-zero.fixture.window-bytes": str(fixture["window_bytes"]),
    }
    config = canonical_json(
        {
            "architecture": "amd64",
            "config": {"Labels": labels},
            "created": "1970-01-01T00:00:00Z",
            "history": [{"created_by": "oci-zero deterministic fixture generator"}],
            "os": "linux",
            "rootfs": {
                "diff_ids": [fixture["decompressed_digest"]],
                "type": "layers",
            },
        }
    )
    config_digest, config_size = write_blob(layout, config)

    manifest = canonical_json(
        {
            "annotations": labels,
            "config": {
                "digest": config_digest,
                "mediaType": "application/vnd.oci.image.config.v1+json",
                "size": config_size,
            },
            "layers": [
                {
                    "annotations": {
                        "io.oci-zero.fixture.window-bytes": str(
                            fixture["window_bytes"]
                        ),
                        "org.opencontainers.image.title": layer_path.name,
                    },
                    "digest": fixture["compressed_digest"],
                    "mediaType": "application/vnd.oci.image.layer.v1.tar+zstd",
                    "size": fixture["compressed_bytes"],
                }
            ],
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "schemaVersion": 2,
        }
    )
    manifest_digest, manifest_size = write_blob(layout, manifest)
    index = {
        "manifests": [
            {
                "annotations": {"org.opencontainers.image.ref.name": name},
                "digest": manifest_digest,
                "mediaType": "application/vnd.oci.image.manifest.v1+json",
                "platform": {"architecture": "amd64", "os": "linux"},
                "size": manifest_size,
            }
        ],
        "mediaType": "application/vnd.oci.image.index.v1+json",
        "schemaVersion": 2,
    }
    (layout / "index.json").write_bytes(canonical_json(index) + b"\n")
    fixture["manifest_digest"] = manifest_digest
    fixture["layout"] = str(layout)


def generate_fixture(output_dir, spec, motif):
    try:
        name, size_mib, window_log = spec.split(":")
        payload_size = int(size_mib) * MIB
        window_log = int(window_log)
    except ValueError as error:
        raise ValueError(f"invalid fixture {spec!r}; expected NAME:SIZE_MIB:WINDOW_LOG") from error
    if not re.fullmatch(r"[a-z0-9][a-z0-9._-]*", name):
        raise ValueError(f"invalid fixture name: {name!r}")
    if payload_size <= 0 or not 10 <= window_log <= 25:
        raise ValueError(f"invalid fixture size or window log: {spec!r}")

    decompressed_size = tar_size(payload_size)
    layer_path = output_dir / f"{name}.tar.zst"
    command = [
        "zstd",
        "-q",
        "-f",
        "-3",
        "--single-thread",
        f"--long={window_log}",
        f"--stream-size={decompressed_size}",
        "-o",
        str(layer_path),
        "-",
    ]
    process = subprocess.Popen(command, stdin=subprocess.PIPE)
    if process.stdin is None:
        raise RuntimeError("zstd did not open stdin")
    writer = HashingWriter(process.stdin)
    info = tarfile.TarInfo(ENTRY)
    info.mode = 0o644
    info.mtime = 0
    info.size = payload_size
    with tarfile.open(fileobj=writer, mode="w|", format=tarfile.USTAR_FORMAT) as archive:
        archive.addfile(info, RepeatingPayload(payload_size, motif))
    writer.flush()
    process.stdin.close()
    if process.wait() != 0:
        raise RuntimeError(f"zstd failed for fixture {name}")
    if writer.size != decompressed_size:
        raise RuntimeError(
            f"tar size mismatch for {name}: expected {decompressed_size}, got {writer.size}"
        )

    compressed_digest, compressed_size = digest_file(layer_path)
    listing = subprocess.run(
        ["zstd", "-lv", str(layer_path)],
        check=True,
        capture_output=True,
        text=True,
    )
    match = re.search(r"Window Size:.*\((\d+) B\)", listing.stdout + listing.stderr)
    window_size = int(match.group(1)) if match else None
    if window_size != 1 << window_log:
        raise RuntimeError(
            f"frame window mismatch for {name}: expected {1 << window_log}, got {window_size}"
        )

    fixture = {
        "compressed_bytes": compressed_size,
        "compressed_digest": compressed_digest,
        "decompressed_bytes": decompressed_size,
        "decompressed_digest": f"sha256:{writer.hash.hexdigest()}",
        "entry": ENTRY,
        "layer": str(layer_path),
        "name": name,
        "package": PACKAGE,
        "payload_bytes": payload_size,
        "window_bytes": window_size,
        "window_log": window_log,
    }
    create_layout(output_dir, fixture, layer_path)
    return fixture


def main():
    parser = argparse.ArgumentParser(
        description="Generate deterministic large tar+zstd OCI benchmark fixtures"
    )
    parser.add_argument(
        "--fixture",
        action="append",
        dest="fixtures",
        metavar="NAME:SIZE_MIB:WINDOW_LOG",
        help="fixture to generate; may be repeated",
    )
    parser.add_argument(
        "--output-dir",
        type=Path,
        default=Path("target/small-window-fixtures"),
    )
    args = parser.parse_args()
    args.output_dir.mkdir(parents=True, exist_ok=True)
    version = subprocess.run(
        ["zstd", "--version"], check=True, capture_output=True, text=True
    ).stdout
    if ZSTD_VERSION not in version:
        raise SystemExit(f"fixture generation requires zstd {ZSTD_VERSION}: {version.strip()}")

    # SHAKE produces the motif in optimized native code. Keeping the motif
    # larger than every declared frame window prevents the repeated payload
    # from collapsing into a tiny network fixture.
    motif = hashlib.shake_256(b"oci-zero-small-window-fixture-v1").digest(MIB)
    fixtures = [
        generate_fixture(args.output_dir, spec, motif)
        for spec in (args.fixtures or DEFAULT_FIXTURES)
    ]
    metadata_path = args.output_dir / "fixtures.json"
    metadata_path.write_text(
        json.dumps({"fixtures": fixtures, "schema_version": 1}, indent=2, sort_keys=True)
        + "\n",
        encoding="utf-8",
    )
    print(metadata_path)


if __name__ == "__main__":
    main()
