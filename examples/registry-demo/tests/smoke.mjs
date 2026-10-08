// Check HTTP metadata and pagination. Rust roundtrips cover graph traversal,
// platform selection, layer verification, and extracted file contents.
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
assert.equal((await get("/v2/", { method: "PUT", body: new Uint8Array([255, 254, 0, 128]) })).status, 405);

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
const zero = await get("/v2/demo/garden/tags/list?n=0");
assert.equal(zero.status, 200);
assert.deepEqual(await zero.json(), { name: "demo/garden", tags: [] });
assert.equal(zero.headers.get("Link"), null);

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

const latest = await payload("demo/garden", "manifests", "latest");
const image = JSON.parse(new TextDecoder().decode(latest.bytes));
for (const descriptor of [image.config, ...image.layers]) {
  await payload("demo/garden", "blobs", descriptor.digest, descriptor);
}
assert.equal((await payload("demo/garden", "manifests", "v2")).actual, latest.actual);
assert.notEqual((await payload("demo/garden", "manifests", "v1")).actual, latest.actual);
const missing = await get("/v2/demo/garden/manifests/missing");
assert.equal(missing.status, 404);
assert.equal((await missing.json()).errors[0].code, "MANIFEST_UNKNOWN");
console.log(`Verified registry probes, pagination, CORS, GET/HEAD metadata, and tag aliases at ${base}`);
