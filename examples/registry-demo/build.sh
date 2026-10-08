#!/bin/sh
set -eu
cd "$(dirname "$0")"
# --target web exposes initSync, which accepts Wrangler's compiled Wasm module.
wasm-pack build --target web --out-dir pkg --release -- --features wasm
