# A pocket OCI registry

One immutable registry, two transports: a local Rust HTTP server and a
Cloudflare Worker running the same Rust router in WebAssembly. No registry
daemon, database, KV, R2, or runtime filesystem is needed. The shared store and
router are `no_std`; the TCP demo uses `std` for sockets and the Wasm bridge
allocates at the JavaScript boundary.

## Run locally

From the repository root:

```sh
cargo run -p oci-zero-registry-demo --features local
```

Open the web explorer and click **Open local demo**, or enter
`http://127.0.0.1:8787` in its registry catalog field. Cross-origin requests work
with normal browser security enabled. You can pass a different bind address:

```sh
cargo run -p oci-zero-registry-demo --features local -- 127.0.0.1:9000
curl http://127.0.0.1:9000/v2/_catalog
curl 'http://127.0.0.1:9000/v2/demo/garden/tags/list?n=2'
```

The TCP adapter bounds request headers and waits, closes each connection, and
handles requests sequentially. It is a development demo; use a proper HTTP
transport for larger or public servers.

## Run or deploy the Cloudflare Worker

Install [wasm-pack](https://rustwasm.github.io/wasm-pack/installer/) and Node.js
22+ (CI uses Node 24), then:

```sh
cd examples/registry-demo
npm ci
npm run dev
# Publish to your own Cloudflare account when ready:
npx wrangler login
npm run deploy
```

Wrangler builds Rust and imports the Wasm module. Paste the resulting
`https://oci-zero-demo.<your-subdomain>.workers.dev` URL into the explorer's
catalog field. Change `name` in `wrangler.toml` if needed. No paid bindings are
used; requests count toward the
[Workers Free request/CPU limits](https://developers.cloudflare.com/workers/platform/limits/).
Deploy is explicit; CI requires no Cloudflare credentials.

## Things to explore

* `demo/garden:v1`: a greeting, MOTD, and seed in one tiny tar layer.
* `demo/garden:v2` and `latest`: identical digests, so the explorer groups aliases.
  The second layer replaces `hello.txt`, removes the seed using a whiteout, and
  adds a flower. Scan files, compare versions, download a file, or export OCI
  and Docker archives.
* `demo/garden:multi`: an amd64/arm64 index with child manifests available by
  digest, including the untagged arm64 image.
* `demo/source:latest`: an OCI source artifact with this checkout's real
  `README.md` and `src/lib.rs` alongside `memfs/note.txt`. Scan or export as OCI.

`build.rs` uses `oci-zero`'s deterministic `TarWriter`, generates image configs
with correct diff IDs, hashes every payload, and embeds the bytes at compile
time. Edits to the included repository files rebuild the data. Requests only
borrow baked bytes; they neither read disk nor rebuild archives.

## Embed the API

```rust
use oci_zero::server::{serve, Content, MemoryStore, Repository, Tag};

let bytes = b"{}"; // Replace with a valid OCI manifest and its referenced blobs.
let manifest = Content::new(bytes, "application/vnd.oci.image.manifest.v1+json");
let tags = [Tag { name: "latest", digest: manifest.digest }];
let manifests = [manifest];
let repositories = [Repository {
    name: "demo/image", tags: &tags, manifests: &manifests, blobs: &[],
}];
let store = MemoryStore(&repositories);
let mut scratch = [0; 1024];
let response = serve(&store, "GET", "/v2/demo/image/manifests/latest", &mut scratch).unwrap();
assert_eq!(response.body, bytes);
```

`MemoryStore` borrows slices from ROM, `include_bytes!` assets, or caller-owned
buffers. `Content::new` hashes once when assembling a store; a known digest can
also be supplied directly if it matches the bytes. Include untagged index
children in `Repository::manifests`. For another memfs, implement `Store` with
indexed name enumeration and repository-scoped manifest/blob lookup.

Send `status`, `media_type`, `content_length`, `digest` (as
`Docker-Content-Digest`) and `next` (formatted as `Link`) through your transport.
Also set `Docker-Distribution-API-Version: registry/2.0` and `Allow` on 405. HEAD
has an empty body but preserves the GET representation length. Keep the backing
bytes and scratch alive while sending the response.

This is a read-only subset of the
[OCI Distribution API](https://github.com/opencontainers/distribution-spec/blob/main/spec.md):
GET/HEAD probes, catalog/tags with `n`/`last` pagination, manifests by tag or
digest, and blobs. OPTIONS supports preflight. No push, deletion, referrers,
byte ranges, or Accept negotiation is implemented. Missing resources return
OCI JSON errors; unsupported methods return 405. Listings sort names without
allocation using repeated scans, suitable for small catalogs.

The public demos reflect `Origin` and allow credentialed reads because the
explorer uses `credentials: include`. Cookies and authorization never grant
access. Add your own access policy when adapting this to private data.

## Verify

```sh
cargo test -p oci-zero --test server
cargo test -p oci-zero-registry-demo --features local --test roundtrip
node --test examples/registry-demo/tests/*.test.mjs
cargo check -p oci-zero-registry-demo --lib --target riscv32imc-unknown-none-elf
```

The round-trip tests start the Rust server on an ephemeral loopback port and use
`RequestPlanner`, `pull`, digest verification, and `VerifiedEntryExtractor` to
download image layers and recover embedded files. They cover both index
architectures, repository files, generated files, and corrupted downloads.
Set `OCI_ZERO_DEMO_REGISTRY_URL` and add `-- --include-ignored` to also run the
same Rust client against a running Worker; CI exercises this with the local Worker.
