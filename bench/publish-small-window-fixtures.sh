#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
FIXTURE_DIR=${FIXTURE_DIR:-"$ROOT_DIR/target/small-window-fixtures"}
PACKAGE=${PACKAGE:-ghcr.io/pawelchcki/oci-zero-small-window-fixtures}

for command in crane python3 zstd; do
    command -v "$command" >/dev/null 2>&1 || {
        echo "publish-small-window-fixtures: required command not found: $command" >&2
        exit 1
    }
done

"$ROOT_DIR/bench/generate-small-window-fixtures.py" --output-dir "$FIXTURE_DIR"

while IFS=$'\t' read -r name layout expected_digest; do
    reference="$PACKAGE:$name"
    existing_digest=$(crane digest "$reference" 2>/dev/null || true)
    if [[ -n $existing_digest && $existing_digest != "$expected_digest" ]]; then
        echo "publish-small-window-fixtures: refusing to replace $reference" >&2
        echo "existing: $existing_digest" >&2
        echo "generated: $expected_digest" >&2
        exit 1
    fi
    if [[ $existing_digest == "$expected_digest" ]]; then
        echo "Already published $reference@$expected_digest" >&2
        continue
    fi

    crane push "$layout" "$reference"
    published_digest=$(crane digest "$reference")
    if [[ $published_digest != "$expected_digest" ]]; then
        echo "publish-small-window-fixtures: digest mismatch after pushing $reference" >&2
        exit 1
    fi
    echo "Published $reference@$published_digest" >&2
done < <(
    python3 - "$FIXTURE_DIR/fixtures.json" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as source:
    for fixture in json.load(source)["fixtures"]:
        print(fixture["name"], fixture["layout"], fixture["manifest_digest"], sep="\t")
PY
)
