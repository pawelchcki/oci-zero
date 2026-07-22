# OCI Zero browser

This directory is both a static browser example and an unpacked Manifest V3
Chrome extension. It uses the same HTML, JavaScript, and `oci-zero` WebAssembly
module in both modes.

## Build

Install [`wasm-pack`](https://rustwasm.github.io/wasm-pack/installer/) and run:

```console
web/build.sh
```

For ordinary browser mode without installing an extension, build the
self-contained page (see [Proxyless single-file build](#proxyless-single-file-build)
below) and open it in a dedicated, CORS-disabled Chrome profile:

```console
open -na "Google Chrome" --args \
  --user-data-dir=/tmp/chrome-no-cors \
  --disable-web-security \
  --app="file://$PWD/dist/proxyless.html"
```

`--user-data-dir` keeps the relaxed profile isolated from your normal browser,
`--disable-web-security` lets the page fetch registries that omit CORS headers,
and `--app` opens it as a standalone window. Point `--app` at any URL that hosts
the page.

This mode talks to registries directly. It sends eligible existing browser
cookies but does not read, store, or request registry credentials. Only use the
CORS-disabled profile for this page.

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

The explorer presents catalog repositories as a flat, virtualized path list.
Repository and tag filters search only pages already loaded in the browser and
report that boundary explicitly. **Load more** fetches one page. With a non-empty
filter, **Search remaining pages** follows pagination sequentially until it is
finished or cancelled; progress and partial results are retained if a request
fails or the registry repeats a pagination cursor.

Tags appear as soon as each tag-name page arrives. Manifest digests are resolved
with at most four concurrent requests only when rows enter the visible virtual
window (including its small overscan area), then equal digests are progressively
combined into alias groups. Until all loaded tags have been visited, digest
filtering and alias grouping are explicitly partial. A deep tag search loads tag
names but does not eagerly resolve every discovered manifest, and changing
repositories cancels obsolete manifest work.

The Distribution registry API has no portable server-side substring search or
portable total-count response. Consequently, an empty result applies to loaded
pages unless an explicit deep search has completed, and the UI never claims a
registry-wide total that the registry did not provide.

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
browsing, virtualized large lists, progressive tag resolution, deep-search
pagination and cancellation, 250-row file pagination, provisional-to-verified
streamed listings, filtering, file download, and rollback after an integrity
failure.

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
intended for environments that run the browser with CORS disabled against a live
registry and may follow Datadog's moving `agent-package:latest` tag. Point it at
a running instance with `OCI_ZERO_LIVE_BASE_URL`.

## Proxyless single-file build

To publish the browser as a single file with no separate static assets, build
the self-contained HTML artifact after `web/build.sh`:

```console
cd web
npm run build:proxyless
```

The result is `web/dist/proxyless.html`; it embeds the stylesheet, application,
scan worker, and WebAssembly module so it can be hosted as one private object or
opened directly from disk. Registry access depends on the target
registry's browser CORS policy, so open it either in the Chrome extension or in
the CORS-disabled Chrome profile shown above.

## GitHub Pages

The `.github/workflows/pages.yml` workflow builds the WebAssembly package and the
self-contained page, then publishes it to GitHub Pages on every push to `main`
that touches `web/` (and on manual dispatch). It runs after Pages is enabled for
the repository with **Build and deployment → Source: GitHub Actions**.

Because the hosted page is a normal HTTPS origin, registry access there only
works for registries that expose browser CORS headers. For registries that do
not, download `index.html` and open it in the CORS-disabled Chrome profile or use
the Chrome extension.
