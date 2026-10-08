// Exercise the real HTTP transport, independent of Node/Worker implementation.
import assert from "node:assert/strict";
import { createHash } from "node:crypto";

const base = process.argv[2];
if (!base) throw new Error("usage: node tests/smoke.mjs http://127.0.0.1:8787");
const origin = "https://explorer.example";
const digest = (bytes) => `sha256:${createHash("sha256").update(bytes).digest("hex")}`;

async function get(path, init = {}) {
  const response = await fetch(new URL(path, base), { ...init, headers: { Origin: origin, ...init.headers } });
  assert.equal(response.headers.get("Access-Control-Allow-Origin"), origin);
  assert.equal(response.headers.get("Access-Control-Allow-Credentials"), "true");
  assert.equal(response.headers.get("Docker-Distribution-API-Version"), "registry/2.0");
  return response;
}

const probe = await get("/v2/");
assert.equal(probe.status, 200);
assert.deepEqual(await probe.json(), {});
const preflight = await get("/v2/demo/garden/tags/list", { method: "OPTIONS", headers: { "Access-Control-Request-Method": "GET" } });
assert.equal(preflight.status, 204);
assert.equal(await preflight.text(), "");
assert.equal((await get("/v2/", { method: "PUT" })).status, 405);

async function pages(path, key) {
  const names = [];
  const seen = new Set();
  let next = path;
  while (next) {
    assert.ok(!seen.has(next), "pagination repeats a cursor");
    seen.add(next);
    const response = await get(next);
    assert.equal(response.status, 200);
    names.push(...(await response.json())[key]);
    next = response.headers.get("Link")?.match(/^<([^>]+)>; rel="next"$/)?.[1];
  }
  return names;
}

assert.deepEqual(await pages("/v2/_catalog?n=1", "repositories"), ["demo/garden", "demo/source"]);
assert.deepEqual(await pages("/v2/demo/garden/tags/list?n=1", "tags"), ["latest", "multi", "v1", "v2"]);

async function payload(repo, kind, reference, descriptor) {
  const path = `/v2/${repo}/${kind}/${reference}`;
  const response = await get(path);
  assert.equal(response.status, 200, path);
  const bytes = new Uint8Array(await response.arrayBuffer());
  const actual = digest(bytes);
  assert.equal(response.headers.get("Docker-Content-Digest"), actual);
  assert.equal(Number(response.headers.get("Content-Length")), bytes.length);
  if (descriptor) {
    assert.equal(actual, descriptor.digest);
    assert.equal(bytes.length, descriptor.size);
  }
  const head = await get(path, { method: "HEAD" });
  assert.equal(head.status, 200);
  assert.equal(head.headers.get("Docker-Content-Digest"), actual);
  assert.equal(Number(head.headers.get("Content-Length")), bytes.length);
  assert.equal((await head.arrayBuffer()).byteLength, 0);
  return { bytes, actual };
}

const visited = new Set();
async function manifest(repo, reference, descriptor) {
  const { bytes, actual } = await payload(repo, "manifests", reference, descriptor);
  const value = JSON.parse(new TextDecoder().decode(bytes));
  if (visited.has(`${repo}@${actual}`)) return actual;
  visited.add(`${repo}@${actual}`);
  if (value.manifests) {
    for (const child of value.manifests) await manifest(repo, child.digest, child);
  } else {
    const config = await payload(repo, "blobs", value.config.digest, value.config);
    const rootfs = JSON.parse(new TextDecoder().decode(config.bytes)).rootfs;
    for (const [index, layer] of value.layers.entries()) {
      const decoded = await payload(repo, "blobs", layer.digest, layer);
      if (rootfs) assert.equal(digest(decoded.bytes), rootfs.diff_ids[index]);
      // The fixtures are uncompressed tar archives with a canonical ending.
      assert.equal(decoded.bytes.length % 512, 0);
      assert.ok(decoded.bytes.slice(-1024).every((byte) => byte === 0));
    }
  }
  return actual;
}

const latest = await manifest("demo/garden", "latest");
assert.equal(await manifest("demo/garden", "v2"), latest);
assert.notEqual(await manifest("demo/garden", "v1"), latest);
await manifest("demo/garden", "multi");
await manifest("demo/source", "latest");
const missing = await get("/v2/demo/garden/manifests/missing");
assert.equal(missing.status, 404);
assert.equal((await missing.json()).errors[0].code, "MANIFEST_UNKNOWN");
console.log(`Verified registry probes, pagination, CORS, HEAD, ${visited.size} manifests, and all referenced blobs at ${base}`);
