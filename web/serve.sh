#!/bin/sh
set -eu

web_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
port=${OCI_ZERO_WEB_PORT:-8000}

case $port in
    ''|*[!0-9]*)
        echo "OCI_ZERO_WEB_PORT must be a port number" >&2
        exit 1
        ;;
esac
if [ "$port" -lt 1 ] || [ "$port" -gt 65535 ]; then
    echo "OCI_ZERO_WEB_PORT must be between 1 and 65535" >&2
    exit 1
fi

docker compose --file "$web_dir/docker-compose.yml" up --build --detach --wait

url="http://localhost:$port/"
echo "OCI Zero Browser is running at $url"
echo "Stop it with: docker compose --file $web_dir/docker-compose.yml down"

if [ "${NO_OPEN:-0}" = 1 ]; then
    exit 0
fi

if [ -n "${BROWSER:-}" ]; then
    if ! browser=$(command -v "$BROWSER" 2>/dev/null); then
        echo "BROWSER does not name an executable: $BROWSER" >&2
        exit 1
    fi
    "$browser" "$url" >/dev/null 2>&1 &
elif [ "$(uname -s)" = Darwin ]; then
    open "$url"
elif command -v xdg-open >/dev/null 2>&1; then
    xdg-open "$url" >/dev/null 2>&1 &
else
    echo "Open $url in a browser."
fi
