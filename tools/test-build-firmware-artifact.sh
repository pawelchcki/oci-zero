#!/bin/sh
set -eu

repository=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf -- "$work"' EXIT INT TERM

printf 'firmware' > "$work/firmware.bin"
mkdir "$work/unrelated"
printf 'keep me' > "$work/unrelated/sentinel"

if "$repository/tools/build-firmware-artifact.sh" \
    --image "$work/firmware.bin:0x10000" \
    --version test \
    --output "$work/unrelated" \
    > "$work/stdout" 2> "$work/stderr"; then
    echo "builder replaced an unrelated output directory" >&2
    exit 1
fi

test "$(cat "$work/unrelated/sentinel")" = "keep me"
grep -q "not an OCI image layout" "$work/stderr"

ln -s "$work/unrelated" "$work/symlink"
if "$repository/tools/build-firmware-artifact.sh" \
    --image "$work/firmware.bin:0x10000" \
    --version test \
    --output "$work/symlink" \
    > "$work/stdout" 2> "$work/stderr"; then
    echo "builder replaced a symlink output path" >&2
    exit 1
fi

test "$(cat "$work/unrelated/sentinel")" = "keep me"
grep -q "symlink output path" "$work/stderr"

for _iteration in 1 2; do
    "$repository/tools/build-firmware-artifact.sh" \
        --image "$work/firmware.bin:0x10000" \
        --version test \
        --output "$work/layout" \
        > "$work/stdout" 2> "$work/stderr"
done

test -f "$work/layout/index.json"
test -f "$work/layout/oci-layout"

printf 'keep layout' > "$work/layout/sentinel"
ln -s "$work/layout" "$work/layout-link"
for _suffix in '' / ///; do
    if "$repository/tools/build-firmware-artifact.sh" \
        --image "$work/firmware.bin:0x10000" \
        --version test \
        --output "$work/layout-link$_suffix" \
        > "$work/stdout" 2> "$work/stderr"; then
        echo "builder followed a symlink to an existing layout" >&2
        exit 1
    fi
    test -L "$work/layout-link"
    test "$(cat "$work/layout/sentinel")" = "keep layout"
    test -f "$work/layout/index.json"
    test -f "$work/layout/oci-layout"
    grep -q "symlink output path" "$work/stderr"
done

"$repository/tools/build-firmware-artifact.sh" \
    --image "$work/firmware.bin:0x10000" \
    --version test \
    --output "$work/layout///" \
    > "$work/stdout" 2> "$work/stderr"
test -f "$work/layout/index.json"
test -f "$work/layout/oci-layout"
