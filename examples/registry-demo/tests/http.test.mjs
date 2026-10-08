import { test } from "node:test";
import assert from "node:assert/strict";
import { registryFetch } from "../http.mjs";

test("credentialed CORS exposes protocol headers and frees the Wasm result", async () => {
  let freed = false;
  const response = registryFetch(new Request("https://demo.example/v2/_catalog?n=1", {
    headers: { Origin: "https://explorer.example" },
  }), (method, target) => {
    assert.equal(method, "GET");
    assert.equal(target, "/v2/_catalog?n=1");
    return {
      status: 200, media_type: "application/json", length: 2,
      body: new TextEncoder().encode("{}"), digest: "sha256:demo",
      link: '</v2/_catalog?n=1&last=a>; rel="next"',
      free() { freed = true; },
    };
  });
  assert.equal(await response.text(), "{}");
  assert.equal(response.headers.get("Access-Control-Allow-Origin"), "https://explorer.example");
  assert.equal(response.headers.get("Access-Control-Allow-Credentials"), "true");
  assert.match(response.headers.get("Access-Control-Expose-Headers"), /Link/);
  assert.equal(response.headers.get("Vary"), "Origin");
  assert.equal(response.headers.get("Docker-Content-Digest"), "sha256:demo");
  assert.ok(freed);
});

test("HEAD and preflight return no body, and writes advertise supported methods", async () => {
  for (const [method, status, length] of [["HEAD", 200, 40], ["OPTIONS", 204, 0], ["PUT", 405, 2]]) {
    const response = registryFetch(new Request("https://demo.example/v2/", { method }), () => ({
      status, length, media_type: "application/json", body: new TextEncoder().encode("{}"),
      free() {},
    }));
    assert.equal(response.status, status);
    assert.equal(response.headers.get("Content-Length"), String(length));
    assert.equal(await response.text(), method === "PUT" ? "{}" : "");
    if (status === 405) assert.equal(response.headers.get("Allow"), "GET, HEAD, OPTIONS");
  }
});

test("dispatch failures remain readable across origins", () => {
  const response = registryFetch(new Request("https://demo.example/v2/", {
    headers: { Origin: "null" },
  }), () => { throw new Error("buffer full"); });
  assert.equal(response.status, 500);
  assert.equal(response.headers.get("Access-Control-Allow-Origin"), "null");
});
