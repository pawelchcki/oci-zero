#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
FIXTURES_FILE="$ROOT_DIR/bench/fixtures.json"
OUTPUT=
TARGET_DIR=${CARGO_TARGET_DIR:-"$ROOT_DIR/target"}
INSTRUCTION_COUNTER=callgrind
SKIP_BUILD=false
FIXTURE_NAME=datadog-10m-w32m
SOURCE_OVERRIDE=

usage() {
    cat <<'EOF'
Usage: bench/e2e-overhead.sh [OPTIONS]

Measure the pinned OCI layer download, verification, decoding, and extraction.

Options:
  --fixture NAME                Fixture from bench/fixtures.json
  --list-fixtures               List available fixtures and exit
  --source PATH_OR_URL          Override the fixture source for local testing
  --output PATH                 JSON result path
  --instruction-counter TYPE   callgrind (default) or perf
  --skip-build                  Reuse the existing release binary
  -h, --help                    Show this help
EOF
}

fail() {
    echo "e2e-overhead: $*" >&2
    exit 1
}

require_command() {
    command -v "$1" >/dev/null 2>&1 || fail "required command not found: $1"
}

read_value() {
    local key=$1
    local file=$2
    awk -F= -v key="$key" '$1 == key { print $2; exit }' "$file"
}

require_unsigned() {
    local value=$1
    local label=$2
    case "$value" in
        ''|*[!0-9]*) fail "could not read $label" ;;
    esac
}

while (($#)); do
    case "$1" in
        --output)
            (($# >= 2)) || fail "--output requires a path"
            OUTPUT=$2
            shift 2
            ;;
        --fixture)
            (($# >= 2)) || fail "--fixture requires a name"
            FIXTURE_NAME=$2
            shift 2
            ;;
        --list-fixtures)
            python3 - "$FIXTURES_FILE" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as source:
    for name in json.load(source)["fixtures"]:
        print(name)
PY
            exit 0
            ;;
        --source)
            (($# >= 2)) || fail "--source requires a path or URL"
            SOURCE_OVERRIDE=$2
            shift 2
            ;;
        --instruction-counter)
            (($# >= 2)) || fail "--instruction-counter requires callgrind or perf"
            INSTRUCTION_COUNTER=$2
            shift 2
            ;;
        --skip-build)
            SKIP_BUILD=true
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            fail "unknown argument: $1"
            ;;
    esac
done

[[ $(uname -s) == Linux ]] || fail \
    "profiling requires Linux; use the E2E overhead GitHub Actions workflow from macOS"
[[ $INSTRUCTION_COUNTER == callgrind || $INSTRUCTION_COUNTER == perf ]] || fail \
    "instruction counter must be callgrind or perf"

for command in cargo git nm python3 rustc valgrind; do
    require_command "$command"
done

mapfile -t FIXTURE < <(
    python3 - "$FIXTURES_FILE" "$FIXTURE_NAME" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as source:
    fixtures = json.load(source)["fixtures"]
try:
    fixture = fixtures[sys.argv[2]]
except KeyError:
    raise SystemExit(f"unknown fixture: {sys.argv[2]}")
for key in (
    "url",
    "entry",
    "compressed_bytes",
    "decompressed_bytes",
    "payload_bytes",
    "history_bytes",
    "compressed_digest",
    "decompressed_digest",
    "reference",
):
    print(fixture[key])
PY
)
[[ ${#FIXTURE[@]} == 9 ]] || fail "could not load fixture: $FIXTURE_NAME"
FIXTURE_URL=${FIXTURE[0]}
if [[ -n $SOURCE_OVERRIDE ]]; then
    FIXTURE_URL=$SOURCE_OVERRIDE
fi
FIXTURE_ENTRY=${FIXTURE[1]}
TRANSFERRED_BYTES=${FIXTURE[2]}
DECODED_BYTES=${FIXTURE[3]}
PAYLOAD_BYTES=${FIXTURE[4]}
HISTORY_BYTES=${FIXTURE[5]}
COMPRESSED_DIGEST=${FIXTURE[6]}
DECOMPRESSED_DIGEST=${FIXTURE[7]}
FIXTURE_REFERENCE=${FIXTURE[8]}
FIXTURE_ARGUMENTS=(
    "$FIXTURE_URL"
    "$FIXTURE_ENTRY"
    "$COMPRESSED_DIGEST"
    "$TRANSFERRED_BYTES"
    "$DECOMPRESSED_DIGEST"
    "$DECODED_BYTES"
    "$HISTORY_BYTES"
)
if [[ -z $OUTPUT ]]; then
    OUTPUT="$ROOT_DIR/target/e2e-overhead/$FIXTURE_NAME/results.json"
fi
[[ -x /usr/bin/time ]] || fail "GNU time is required at /usr/bin/time"
if [[ $INSTRUCTION_COUNTER == perf ]]; then
    require_command perf
fi

if [[ $SKIP_BUILD == false ]]; then
    echo "Building the instrumented release binary..." >&2
    cargo build \
        --manifest-path "$ROOT_DIR/Cargo.toml" \
        --release \
        -p oci-zero-no-std-extract \
        --features bench-metrics
fi

BINARY="$TARGET_DIR/release/oci-zero-no-std-extract"
[[ -x $BINARY ]] || fail "release binary not found: $BINARY"

DYNAMIC_SYMBOLS=$(nm -D "$BINARY")
if awk '
    $1 == "U" && $2 ~ /^(malloc|calloc|realloc|free)(@|$)/ { found = 1 }
    END { exit !found }
' <<<"$DYNAMIC_SYMBOLS"; then
    fail "binary imports a native heap allocator; peak_native_heap_bytes cannot be reported as zero"
fi
if ! awk '
    $2 == "T" && $3 == "calloc" { calloc_found = 1 }
    $2 == "T" && $3 == "free" { free_found = 1 }
    END { exit !(calloc_found && free_found) }
' <<<"$DYNAMIC_SYMBOLS"; then
    fail "binary does not export its fixed-arena calloc/free implementation"
fi

RESULT_DIR=$(dirname -- "$OUTPUT")
mkdir -p "$RESULT_DIR"
TIME_OUTPUT="$RESULT_DIR/time.txt"
RSS_STDERR="$RESULT_DIR/rss.stderr"
MASSIF_OUTPUT="$RESULT_DIR/massif.out"
MASSIF_STDERR="$RESULT_DIR/massif.stderr"
COUNTER_OUTPUT="$RESULT_DIR/${INSTRUCTION_COUNTER}.out"
COUNTER_STDERR="$RESULT_DIR/${INSTRUCTION_COUNTER}.stderr"

echo "Measuring wall time, peak RSS, and fixed-arena high-water..." >&2
LC_ALL=C /usr/bin/time \
    --output "$TIME_OUTPUT" \
    --format 'wall_seconds=%e
user_seconds=%U
system_seconds=%S
peak_rss_kib=%M' \
    "$BINARY" "${FIXTURE_ARGUMENTS[@]}" --metrics \
    >/dev/null 2>"$RSS_STDERR"

WALL_SECONDS=$(read_value wall_seconds "$TIME_OUTPUT")
USER_SECONDS=$(read_value user_seconds "$TIME_OUTPUT")
SYSTEM_SECONDS=$(read_value system_seconds "$TIME_OUTPUT")
PEAK_RSS_KIB=$(read_value peak_rss_kib "$TIME_OUTPUT")
TLS_ARENA_PEAK_BYTES=$(read_value oci_zero_metric_tls_arena_peak_bytes "$RSS_STDERR")
TLS_ARENA_CAPACITY_BYTES=$(read_value oci_zero_metric_tls_arena_capacity_bytes "$RSS_STDERR")
DECODER_STATIC_BUFFER_BYTES=$(read_value oci_zero_metric_decoder_static_buffer_bytes "$RSS_STDERR")
DECODER_HISTORY_BYTES=$(read_value oci_zero_metric_decoder_history_bytes "$RSS_STDERR")
require_unsigned "$PEAK_RSS_KIB" "peak RSS"
require_unsigned "$TLS_ARENA_PEAK_BYTES" "TLS arena high-water"
require_unsigned "$TLS_ARENA_CAPACITY_BYTES" "TLS arena capacity"
require_unsigned "$DECODER_STATIC_BUFFER_BYTES" "decoder static buffer size"
require_unsigned "$DECODER_HISTORY_BYTES" "decoder history size"
[[ $DECODER_HISTORY_BYTES == "$HISTORY_BYTES" ]] || fail "decoder history does not match fixture"
PEAK_RSS_BYTES=$((PEAK_RSS_KIB * 1024))

echo "Measuring peak live allocation demand with Massif..." >&2
valgrind \
    --quiet \
    --tool=massif \
    --stacks=yes \
    --time-unit=i \
    --massif-out-file="$MASSIF_OUTPUT" \
    "$BINARY" "${FIXTURE_ARGUMENTS[@]}" \
    >/dev/null 2>"$MASSIF_STDERR"

read -r MASSIF_PEAK_LIVE_BYTES MASSIF_PEAK_LIVE_PAYLOAD_BYTES PEAK_STACK_BYTES < <(
    awk -F= '
        $1 == "mem_heap_B" { heap = $2; if (heap > peak_heap) peak_heap = heap }
        $1 == "mem_heap_extra_B" {
            total = heap + $2
            if (total > peak_total) peak_total = total
        }
        $1 == "mem_stacks_B" { if ($2 > peak_stack) peak_stack = $2 }
        END { printf "%.0f %.0f %.0f\n", peak_total, peak_heap, peak_stack }
    ' "$MASSIF_OUTPUT"
)
require_unsigned "$MASSIF_PEAK_LIVE_BYTES" "Massif peak live allocation"
require_unsigned "$MASSIF_PEAK_LIVE_PAYLOAD_BYTES" "Massif peak live allocation payload"
require_unsigned "$PEAK_STACK_BYTES" "peak stack"

if [[ $INSTRUCTION_COUNTER == callgrind ]]; then
    echo "Counting deterministic user-space instructions with Callgrind..." >&2
    valgrind \
        --quiet \
        --tool=callgrind \
        --callgrind-out-file="$COUNTER_OUTPUT" \
        "$BINARY" "${FIXTURE_ARGUMENTS[@]}" \
        >/dev/null 2>"$COUNTER_STDERR"
    INSTRUCTIONS=$(awk '/^summary:/ { print $2; exit }' "$COUNTER_OUTPUT")
    INSTRUCTION_SOURCE=callgrind_ir
else
    echo "Counting retired user-space instructions with perf..." >&2
    perf stat \
        --no-big-num \
        --field-separator ';' \
        --event instructions:u \
        --output "$COUNTER_OUTPUT" \
        -- \
        "$BINARY" "${FIXTURE_ARGUMENTS[@]}" \
        >/dev/null 2>"$COUNTER_STDERR"
    INSTRUCTIONS=$(awk -F';' '
        $3 ~ /^instructions/ {
            gsub(/^[[:space:]]+|[[:space:]]+$/, "", $1)
            print $1
            exit
        }
    ' "$COUNTER_OUTPUT")
    INSTRUCTION_SOURCE=perf_retired_user
fi
require_unsigned "$INSTRUCTIONS" "instruction count"

BINARY_FILE_BYTES=$(stat -c %s "$BINARY")
GIT_COMMIT=$(git -C "$ROOT_DIR" rev-parse HEAD)
if [[ -n $(git -C "$ROOT_DIR" status --porcelain) ]]; then
    GIT_DIRTY=true
else
    GIT_DIRTY=false
fi

SYSTEM_ARCH=$(uname -m)
SYSTEM_KERNEL=$(uname -r)
RUSTC_VERSION=$(rustc --version)
export OUTPUT FIXTURE_NAME FIXTURE_URL FIXTURE_ENTRY FIXTURE_REFERENCE
export TRANSFERRED_BYTES DECODED_BYTES PAYLOAD_BYTES HISTORY_BYTES
export COMPRESSED_DIGEST DECOMPRESSED_DIGEST WALL_SECONDS USER_SECONDS SYSTEM_SECONDS
export PEAK_RSS_BYTES MASSIF_PEAK_LIVE_BYTES MASSIF_PEAK_LIVE_PAYLOAD_BYTES PEAK_STACK_BYTES
export TLS_ARENA_PEAK_BYTES TLS_ARENA_CAPACITY_BYTES DECODER_STATIC_BUFFER_BYTES
export DECODER_HISTORY_BYTES
export INSTRUCTIONS INSTRUCTION_SOURCE BINARY_FILE_BYTES GIT_COMMIT GIT_DIRTY
export SYSTEM_ARCH SYSTEM_KERNEL RUSTC_VERSION

python3 - <<'PY'
import json
import os

def integer(name):
    return int(os.environ[name])


def floating(name):
    return float(os.environ[name])

wire_bytes = integer("TRANSFERRED_BYTES")
decoded_bytes = integer("DECODED_BYTES")
instructions = integer("INSTRUCTIONS")
peak_rss = integer("PEAK_RSS_BYTES")

result = {
    "schema_version": 1,
    "fixture": {
        "name": os.environ["FIXTURE_NAME"],
        "reference": os.environ["FIXTURE_REFERENCE"],
        "url": os.environ["FIXTURE_URL"],
        "entry": os.environ["FIXTURE_ENTRY"],
        "compressed_digest": os.environ["COMPRESSED_DIGEST"],
        "decompressed_digest": os.environ["DECOMPRESSED_DIGEST"],
        "transferred_bytes": wire_bytes,
        "decoded_bytes": decoded_bytes,
        "payload_bytes": integer("PAYLOAD_BYTES"),
        "history_bytes": integer("HISTORY_BYTES"),
        "operation": "HTTPS download, TLS, SHA-256 verification, Zstandard decode, tar scan, extraction",
    },
    "build": {
        "git_commit": os.environ["GIT_COMMIT"],
        "git_dirty": os.environ["GIT_DIRTY"] == "true",
        "rustc": os.environ["RUSTC_VERSION"],
        "binary_file_bytes": integer("BINARY_FILE_BYTES"),
    },
    "system": {
        "os": "linux",
        "architecture": os.environ["SYSTEM_ARCH"],
        "kernel": os.environ["SYSTEM_KERNEL"],
    },
    "measurements": {
        "wall_seconds": floating("WALL_SECONDS"),
        "user_seconds": floating("USER_SECONDS"),
        "system_seconds": floating("SYSTEM_SECONDS"),
        "peak_rss_bytes": peak_rss,
        "peak_native_heap_bytes": 0,
        "massif_peak_live_allocation_bytes": integer("MASSIF_PEAK_LIVE_BYTES"),
        "massif_peak_live_allocation_payload_bytes": integer("MASSIF_PEAK_LIVE_PAYLOAD_BYTES"),
        "peak_stack_bytes": integer("PEAK_STACK_BYTES"),
        "tls_arena_peak_bytes": integer("TLS_ARENA_PEAK_BYTES"),
        "tls_arena_capacity_bytes": integer("TLS_ARENA_CAPACITY_BYTES"),
        "decoder_static_buffer_bytes": integer("DECODER_STATIC_BUFFER_BYTES"),
        "decoder_history_bytes": integer("DECODER_HISTORY_BYTES"),
        "instructions": instructions,
        "instruction_source": os.environ["INSTRUCTION_SOURCE"],
    },
    "derived": {
        "instructions_per_transferred_byte": instructions / wire_bytes,
        "instructions_per_decoded_byte": instructions / decoded_bytes,
        "peak_rss_bytes_per_decoded_byte": peak_rss / decoded_bytes,
    },
}

with open(os.environ["OUTPUT"], "w", encoding="utf-8") as output:
    json.dump(result, output, indent=2, sort_keys=True)
    output.write("\n")
PY

cat "$OUTPUT"
