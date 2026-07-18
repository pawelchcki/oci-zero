# OCI Zero browser

This directory is both a static browser example and an unpacked Manifest V3
Chrome extension. It uses the same HTML, JavaScript, and `oci-zero` WebAssembly
module in both modes.

## Build

Install [`wasm-pack`](https://rustwasm.github.io/wasm-pack/installer/) and run:

```console
web/build.sh
```

For ordinary browser mode without installing an extension, build and start the
containerized server in the background:

```console
web/serve.sh
```

The helper opens <http://localhost:8000>. Set `OCI_ZERO_WEB_PORT` to use another
host port or `NO_OPEN=1` to leave the browser closed. Stop the server with:

```console
docker compose --file web/docker-compose.yml down
```

The container includes a loopback-only, token-protected proxy for registry and
anonymous token-service requests, so this mode does not depend on registry CORS
headers. It does not forward browser cookies or Docker credentials.

## Load the Chrome extension

Run the install helper to install `wasm-pack` with Cargo when needed, build the
extension, and open Chrome's extension page:

```console
web/install.sh
```

Then enable Developer mode, choose **Load unpacked**, select this `web`
directory, and click the extension action to open the full-page browser. Set
`CHROME` to a Chrome or Chromium executable if it is not found automatically.

The extension requests registry, anonymous token-service, and redirected blob
origins one at a time. It sends eligible existing browser cookies but does not
read, store, or request registry credentials. The manifest deliberately has no
content scripts and no `cookies` permission.

The explorer groups catalog repositories by namespace and groups tags whose
resolved manifest bytes have the same digest. A group's immutable version is its
title; moving aliases such as `latest` are shown as chips. Both repository and
version searches cover the currently loaded pages (use **Load more** to extend
them).

**Scan files** streams each supported archive layer through a dedicated worker and shows provisional
filesystem entries while the layer is still downloading. File payload blocks are
discarded while listing, and provisional changes are rolled back if the compressed
digest, size, diff ID, or tar ending fails verification.
Canonical OCI/Docker layers and vendor media types ending in `.tar`, `.tar+gzip`,
`.tar.gzip`, `.tar+zstd`, or `.tar.zstd` are supported.

Image configs use strict compressed digest, size, and `rootfs.diff_ids`
verification. Package and other artifact configs without image diff IDs still
verify the compressed descriptor digest and size, while retaining decoded-byte
accounting. Unsupported payload media types are shown as skipped and do not
prevent later supported archive layers from being scanned. Docker export remains
limited to valid image configs with complete diff IDs; OCI export works for both.

Streaming scans do not have a whole-layer blob limit. Whole-image exports and
extracted files remain limited to 256 MiB in this example. Layer indexes are
limited to 200,000 entries and Zstandard windows to 256 MiB.

## Browser tests

The Playwright suite runs the real page, worker, and WebAssembly module against
a deterministic local OCI registry fixture. It covers catalog and manifest
browsing, provisional-to-verified streamed listings, filtering, file download,
and rollback after an integrity failure.

Build the WebAssembly package, install the test dependency, and run the suite:

```console
web/build.sh
cd web
npm ci
npm run test:e2e
```

The configuration uses an installed Chrome or Chromium when available. Set
`PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH` to select another executable, or run
`npx playwright install chromium` to install Playwright's browser.

An optional live package check can be enabled with `OCI_ZERO_LIVE_E2E=1`; it is
intended for environments that run the browser through the local proxy and may
follow Datadog's moving `agent-package:latest` tag.

## Proxyless single-file build

To publish the browser without its local proxy or separate static assets, build
the self-contained HTML artifact after `web/build.sh`:

```console
cd web
npm run build:proxyless
```

The result is `web/dist/proxyless.html`; it embeds the stylesheet, application,
scan worker, and WebAssembly module so it can be hosted as one private Chonk
object. Registry access still depends on the target registry's browser CORS
policy.
