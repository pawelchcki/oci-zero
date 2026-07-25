#!/bin/sh
# Build an OCI image layout holding one ESP32 firmware image.
#
# The layout directory is the single source of truth for the firmware artifact:
# `crane push <dir> <image>` reads it as an OCI layout, and `tar -cf - -C <dir> .`
# turns the same bytes into the release asset the browser flasher consumes. The
# registry and the release therefore always agree, digest for digest.
#
#   layout/
#     oci-layout
#     index.json                  -> manifest descriptor
#     blobs/sha256/<manifest>     artifactType application/vnd.oci-zero.firmware.v1+json
#     blobs/sha256/<config>       application/vnd.oci-zero.firmware.config.v1+json
#     blobs/sha256/<layer>        application/vnd.oci-zero.firmware.layer.v1.tar+gzip
#
# Two constraints are load-bearing and must survive edits:
#
#  * The layer media type has to keep a `.tar+gzip` or `.tar+zstd` suffix.
#    `browser_encoding` in web/src/lib.rs dispatches on that suffix, so a vendor
#    media type is only decodable in the browser if it ends that way.
#  * gzip is the default, not zstd. The device-side budget (see
#    examples/esp32c3-ota) cannot spare a Zstandard window next to the Matter
#    stack. `--compression zstd` exists so that can flip if measurements allow.
#
# The config blob uses a vendor media type, so it carries no `rootfs.diff_ids`;
# consumers verify the layer against its compressed descriptor digest only.
set -eu

usage() {
    cat >&2 <<'EOF'
Usage: build-firmware-artifact.sh --image PATH:OFFSET --version VERSION [options]

Required:
  --image PATH:OFFSET    an image to flash and where it belongs. Repeatable, in
                         flash order. OFFSET is decimal or 0x-prefixed hex. The
                         basename of PATH becomes its path inside the layer, so
                         `--image firmware.bin:0x10000` is the usual app-only
                         artifact and adding `--image bootloader.bin:0x0
                         --image partition-table.bin:0x8000` makes the artifact
                         able to provision a blank board.
  --version VERSION      artifact version; the single source for the OCI
                         annotation, esp_app_desc and Matter SoftwareVersionString

Options:
  --output DIR           layout directory to create (default: ./layout)
  --revision SHA         git revision annotation (default: git rev-parse HEAD)
  --chip NAME            target chip (default: esp32c3)
  --target TRIPLE        Rust target (default: riscv32imc-unknown-none-elf)
  --compression gzip|zstd  layer compression (default: gzip)
  --summary PATH         write KEY=value results here (for $GITHUB_OUTPUT)
EOF
    exit 2
}

version=
revision=
output=layout
chip=esp32c3
target=riscv32imc-unknown-none-elf
compression=gzip
summary=
# Parallel newline-separated lists; index N of one lines up with index N of the
# others. POSIX sh has no arrays and the layer needs a stable, ordered walk.
image_paths=
image_names=
image_offsets=

# Normalises a decimal or 0x-prefixed offset to decimal, or fails.
normalize_offset() {
    case "$1" in
        0x*|0X*)
            _rest=${1#??}
            case "$_rest" in
                ''|*[!0-9a-fA-F]*) return 1 ;;
            esac
            printf '%d\n' "$1"
            ;;
        ''|*[!0-9]*) return 1 ;;
        *) printf '%s\n' "$1" ;;
    esac
}

add_image() {
    _spec=$1
    case "$_spec" in
        *:*) ;;
        *) echo "--image needs PATH:OFFSET, got: $_spec" >&2; exit 1 ;;
    esac
    # Split on the *last* colon so Windows-style or otherwise colon-bearing
    # paths still work.
    _offset=${_spec##*:}
    _path=${_spec%:*}
    [ -f "$_path" ] || { echo "image not found: $_path" >&2; exit 1; }
    _offset=$(normalize_offset "$_offset") || {
        echo "--image offset must be decimal or 0x hex, got: ${_spec##*:}" >&2
        exit 1
    }
    _name=${_path##*/}
    image_paths="${image_paths}${_path}
"
    image_names="${image_names}${_name}
"
    image_offsets="${image_offsets}${_offset}
"
}

while [ $# -gt 0 ]; do
    case "$1" in
        --image) add_image "${2?missing value for --image}"; shift 2 ;;
        --version) version=${2?missing value for --version}; shift 2 ;;
        --revision) revision=${2?missing value for --revision}; shift 2 ;;
        --output) output=${2?missing value for --output}; shift 2 ;;
        --chip) chip=${2?missing value for --chip}; shift 2 ;;
        --target) target=${2?missing value for --target}; shift 2 ;;
        --compression) compression=${2?missing value for --compression}; shift 2 ;;
        --summary) summary=${2?missing value for --summary}; shift 2 ;;
        -h|--help) usage ;;
        *) echo "unknown argument: $1" >&2; usage ;;
    esac
done

[ -n "$image_paths" ] || usage
[ -n "$version" ] || usage
command -v jq >/dev/null 2>&1 || { echo "jq is required" >&2; exit 1; }
command -v python3 >/dev/null 2>&1 || { echo "python3 is required" >&2; exit 1; }

case "$compression" in
    gzip) layer_suffix="tar+gzip" ;;
    zstd)
        layer_suffix="tar+zstd"
        command -v zstd >/dev/null 2>&1 || { echo "zstd is required for --compression zstd" >&2; exit 1; }
        ;;
    *) echo "--compression must be gzip or zstd: $compression" >&2; exit 1 ;;
esac

if [ -z "$revision" ]; then
    revision=$(git rev-parse HEAD 2>/dev/null || echo unknown)
fi

artifact_type="application/vnd.oci-zero.firmware.v1+json"
config_type="application/vnd.oci-zero.firmware.config.v1+json"
layer_type="application/vnd.oci-zero.firmware.layer.v1.$layer_suffix"
manifest_type="application/vnd.oci.image.manifest.v1+json"
index_type="application/vnd.oci.image.index.v1+json"

sha256_hex() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    else
        shasum -a 256 "$1" | cut -d' ' -f1
    fi
}

blobs="$output/blobs/sha256"
rm -rf "$output"
mkdir -p "$blobs"

# Adds $1 to the blob store and prints its hex digest.
add_blob() {
    _hex=$(sha256_hex "$1")
    mv -f "$1" "$blobs/$_hex"
    echo "$_hex"
}

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT INT TERM

# The layer tar is written by tarfile rather than tar(1) so the bytes do not
# depend on whether GNU tar or bsdtar is installed: uid/gid, names and mtime are
# pinned, and gzip is asked for a zero mtime. Identical inputs therefore produce
# an identical digest on a laptop and in CI.
printf '%s' "$image_paths" > "$work/paths"
printf '%s' "$image_names" > "$work/names"
printf '%s' "$image_offsets" > "$work/offsets"
python3 - "$work/paths" "$work/names" "$work/layer.tar" <<'PY'
import io, sys, tarfile

path_list, name_list, destination = sys.argv[1:4]
paths = open(path_list).read().splitlines()
names = open(name_list).read().splitlines()

with tarfile.open(destination, "w", format=tarfile.USTAR_FORMAT) as archive:
    for path, name in zip(paths, names):
        with open(path, "rb") as image:
            payload = image.read()
        info = tarfile.TarInfo(name)
        info.size = len(payload)
        info.mode = 0o644
        info.mtime = 0
        info.uid = 0
        info.gid = 0
        info.uname = ""
        info.gname = ""
        archive.addfile(info, io.BytesIO(payload))
PY

case "$compression" in
    gzip)
        python3 - "$work/layer.tar" "$work/layer.blob" <<'PY'
import gzip, shutil, sys

source, destination = sys.argv[1:3]
with open(source, "rb") as raw, open(destination, "wb") as out:
    # mtime=0 keeps the gzip header free of a build timestamp.
    with gzip.GzipFile(filename="", mode="wb", compresslevel=9, fileobj=out, mtime=0) as encoder:
        shutil.copyfileobj(raw, encoder)
PY
        ;;
    zstd)
        zstd -19 -q -o "$work/layer.blob" "$work/layer.tar"
        ;;
esac

layer_size=$(wc -c < "$work/layer.blob" | tr -d ' ')
layer_hex=$(add_blob "$work/layer.blob")

# Flash geometry lives in the config so the browser flasher stays generic
# instead of hardcoding ESP32-C3 offsets.
jq -nc \
    --arg chip "$chip" \
    --arg target "$target" \
    --rawfile names "$work/names" \
    --rawfile offsets "$work/offsets" \
    '{
        chip: $chip,
        target: $target,
        entries: [
            ($names | rtrimstr("\n") | split("\n")) as $paths
            | ($offsets | rtrimstr("\n") | split("\n") | map(tonumber)) as $at
            | range($paths | length)
            | {path: $paths[.], offset: $at[.]}
        ]
    }' \
    > "$work/config.json"
config_size=$(wc -c < "$work/config.json" | tr -d ' ')
config_hex=$(add_blob "$work/config.json")

jq -nc \
    --arg manifestType "$manifest_type" \
    --arg artifactType "$artifact_type" \
    --arg configType "$config_type" \
    --arg configDigest "sha256:$config_hex" \
    --argjson configSize "$config_size" \
    --arg layerType "$layer_type" \
    --arg layerDigest "sha256:$layer_hex" \
    --argjson layerSize "$layer_size" \
    --arg version "$version" \
    --arg revision "$revision" \
    --arg chip "$chip" \
    '{
        schemaVersion: 2,
        mediaType: $manifestType,
        artifactType: $artifactType,
        config: {mediaType: $configType, digest: $configDigest, size: $configSize},
        layers: [{mediaType: $layerType, digest: $layerDigest, size: $layerSize}],
        annotations: {
            "org.opencontainers.image.version": $version,
            "org.opencontainers.image.revision": $revision,
            "vnd.oci-zero.firmware.chip": $chip
        }
    }' \
    > "$work/manifest.json"
manifest_size=$(wc -c < "$work/manifest.json" | tr -d ' ')
manifest_hex=$(add_blob "$work/manifest.json")

printf '{"imageLayoutVersion":"1.0.0"}' > "$output/oci-layout"

jq -nc \
    --arg indexType "$index_type" \
    --arg manifestType "$manifest_type" \
    --arg artifactType "$artifact_type" \
    --arg digest "sha256:$manifest_hex" \
    --argjson size "$manifest_size" \
    --arg version "$version" \
    --arg revision "$revision" \
    --arg chip "$chip" \
    '{
        schemaVersion: 2,
        mediaType: $indexType,
        manifests: [{
            mediaType: $manifestType,
            artifactType: $artifactType,
            digest: $digest,
            size: $size,
            annotations: {
                "org.opencontainers.image.version": $version,
                "org.opencontainers.image.revision": $revision,
                "vnd.oci-zero.firmware.chip": $chip
            }
        }]
    }' \
    > "$output/index.json"

if [ -n "$summary" ]; then
    {
        echo "version=$version"
        echo "revision=$revision"
        echo "chip=$chip"
        echo "layer_media_type=$layer_type"
        echo "manifest_digest=sha256:$manifest_hex"
        echo "manifest_size=$manifest_size"
        echo "config_digest=sha256:$config_hex"
        echo "layer_digest=sha256:$layer_hex"
        echo "layer_size=$layer_size"
    } > "$summary"
fi

{
    echo "Built OCI layout $output"
    echo "  version          $version"
    echo "  revision         $revision"
    echo "  chip             $chip"
    echo "  manifest         sha256:$manifest_hex ($manifest_size bytes)"
    echo "  config           sha256:$config_hex ($config_size bytes)"
    echo "  layer            sha256:$layer_hex ($layer_size bytes, $layer_type)"
    printf '%s' "$image_paths" | while IFS= read -r image_path; do
        [ -n "$image_path" ] || continue
        printf '  image            %s -> sha256:%s (%s bytes)\n' \
            "${image_path##*/}" "$(sha256_hex "$image_path")" \
            "$(wc -c < "$image_path" | tr -d ' ')"
    done
} >&2
