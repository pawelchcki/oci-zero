#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
IMAGE=oci-zero-e2e-overhead

docker build \
    --file "$ROOT_DIR/bench/Dockerfile" \
    --tag "$IMAGE" \
    "$ROOT_DIR"

docker run \
    --rm \
    --user "$(id -u):$(id -g)" \
    --env CARGO_TARGET_DIR=/work/target/e2e-linux \
    --volume "$ROOT_DIR:/work" \
    --workdir /work \
    "$IMAGE" \
    "$@"
