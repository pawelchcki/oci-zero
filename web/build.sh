#!/bin/sh
set -eu

cd "$(dirname "$0")"

if ! command -v wasm-pack >/dev/null 2>&1; then
    echo "wasm-pack is required; run web/install.sh to install it and build." >&2
    exit 1
fi

wasm-pack build . --target web --out-dir pkg --release --no-pack

echo "Built web/pkg. Serve this directory or load it unpacked in Chrome."
