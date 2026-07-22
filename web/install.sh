#!/bin/sh
set -eu

web_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)

if ! command -v wasm-pack >/dev/null 2>&1; then
    if ! command -v cargo >/dev/null 2>&1; then
        echo "wasm-pack is missing and Cargo is not available to install it." >&2
        exit 1
    fi

    echo "wasm-pack was not found; installing it with Cargo..."
    cargo install wasm-pack --locked

    cargo_bin_dir=${CARGO_INSTALL_ROOT:-${CARGO_HOME:-$HOME/.cargo}}/bin
    PATH=$cargo_bin_dir:$PATH
    export PATH
fi

"$web_dir/build.sh"

extensions_url=chrome://extensions

if [ -n "${CHROME:-}" ]; then
    if ! chrome=$(command -v "$CHROME" 2>/dev/null); then
        echo "CHROME does not name an executable: $CHROME" >&2
        exit 1
    fi
    "$chrome" "$extensions_url" >/dev/null 2>&1 &
elif [ "$(uname -s)" = Darwin ]; then
    chrome_app=
    for candidate in "Google Chrome" "Google Chrome Canary" Chromium; do
        if [ -d "/Applications/$candidate.app" ] || [ -d "$HOME/Applications/$candidate.app" ]; then
            chrome_app=$candidate
            break
        fi
    done

    if [ -z "$chrome_app" ]; then
        echo "Chrome was not found. Open $extensions_url manually." >&2
        echo "Then load this unpacked extension: $web_dir" >&2
        exit 1
    fi

    open -a "$chrome_app" "$extensions_url"
else
    chrome=
    for candidate in google-chrome-stable google-chrome chromium chromium-browser; do
        if chrome=$(command -v "$candidate" 2>/dev/null); then
            break
        fi
    done

    if [ -z "$chrome" ]; then
        echo "Chrome or Chromium was not found. Open $extensions_url manually." >&2
        echo "Then load this unpacked extension: $web_dir" >&2
        exit 1
    fi

    "$chrome" "$extensions_url" >/dev/null 2>&1 &
fi

echo "In Chrome, enable Developer mode and choose Load unpacked."
echo "Select: $web_dir"
