import { createHash } from "node:crypto";
import { gzipSync } from "node:zlib";
import { createReadStream, existsSync } from "node:fs";
import { stat } from "node:fs/promises";
import { createServer } from "node:http";
import { dirname, extname, join, normalize } from "node:path";
import { fileURLToPath } from "node:url";

const webRoot = dirname(dirname(fileURLToPath(import.meta.url)));
const port = Number(process.env.OCI_ZERO_PLAYWRIGHT_PORT || 4173);
const largeContents = Buffer.alloc(512 * 1024, 0x61);
const layer = tar([
  ["etc/hello.txt", Buffer.from("hello from the streamed layer\n")],
  ["var/cache/large.bin", largeContents],
]);
const layerDigest = digest(layer);
const corruptDigest = `sha256:${"00".repeat(32)}`;
const config = json({
  architecture: "amd64",
  os: "linux",
  rootfs: { type: "layers", diff_ids: [layerDigest] },
  config: { Labels: { fixture: "amd64" } },
});
const configDigest = digest(config);
const armConfig = json({
  architecture: "arm64",
  os: "linux",
  rootfs: { type: "layers", diff_ids: [layerDigest] },
  config: { Labels: { fixture: "arm64" } },
});
const armConfigDigest = digest(armConfig);
const amdManifest = imageManifest(configDigest, config.length, [{
  mediaType: "application/vnd.example.layer.v1.tar",
  digest: layerDigest,
  size: layer.length,
}]);
const armManifest = imageManifest(armConfigDigest, armConfig.length, [{
  mediaType: "application/vnd.example.layer.v1.tar",
  digest: layerDigest,
  size: layer.length,
}]);
const amdManifestDigest = digest(amdManifest);
const armManifestDigest = digest(armManifest);
const multiPlatformIndex = json({
  schemaVersion: 2,
  mediaType: "application/vnd.oci.image.index.v1+json",
  manifests: [
    {
      mediaType: "application/vnd.oci.image.manifest.v1+json",
      digest: amdManifestDigest,
      size: amdManifest.length,
      platform: { os: "linux", architecture: "amd64" },
    },
    {
      mediaType: "application/vnd.oci.image.manifest.v1+json",
      digest: armManifestDigest,
      size: armManifest.length,
      platform: { os: "linux", architecture: "arm64", variant: "v8" },
    },
  ],
});
const lowerLayer = tar([
  ["keep.txt", Buffer.from("kept from lower layer\n")],
  ["remove.txt", Buffer.from("removed by whiteout\n")],
  ["opaque/old.txt", Buffer.from("removed by opaque marker\n")],
  ["shared/target.txt", Buffer.from("hard-link target contents\n")],
]);
const upperLayer = tar([
  [".wh.remove.txt", Buffer.alloc(0)],
  ["opaque/.wh..wh..opq", Buffer.alloc(0)],
  ["opaque/new.txt", Buffer.from("new opaque contents\n")],
  { path: "links/symlink.txt", type: "2", linkTarget: "../shared/target.txt" },
  { path: "links/hard.txt", type: "1", linkTarget: "shared/target.txt" },
  { path: "links/dangling.txt", type: "1", linkTarget: "missing.txt" },
]);
const lowerLayerDigest = digest(lowerLayer);
const upperLayerDigest = digest(upperLayer);
const overlayConfig = json({
  architecture: "amd64",
  os: "linux",
  rootfs: { type: "layers", diff_ids: [lowerLayerDigest, upperLayerDigest] },
  config: { Labels: { fixture: "overlay" } },
});
const overlayConfigDigest = digest(overlayConfig);
const overlayManifest = imageManifest(overlayConfigDigest, overlayConfig.length, [
  { mediaType: "application/vnd.oci.image.layer.v1.tar", digest: lowerLayerDigest, size: lowerLayer.length },
  { mediaType: "application/vnd.oci.image.layer.v1.tar", digest: upperLayerDigest, size: upperLayer.length },
]);
const packageArchive = gzipSync(tar([["etc/datadog-agent/application_monitoring.yaml.example", Buffer.from("# Datadog APM configuration\n")]]));
const packageArchiveDigest = digest(packageArchive);
const installer = Buffer.from("unsupported package installer");
const installerDigest = digest(installer);
// Package configs may carry a rootfs-looking field whose value is not an OCI
// uncompressed tar diff ID. Browsing must not apply image verification to it.
const packageConfig = json({
  package: "datadog-agent",
  rootfs: { type: "package", diff_ids: [packageArchiveDigest] },
});
const packageConfigDigest = digest(packageConfig);

const server = createServer(async (request, response) => {
  const url = new URL(request.url, `http://${request.headers.host}`);
  if (url.pathname === "/healthz") return send(response, 200, "text/plain", Buffer.from("ok\n"));
  if (url.pathname === "/proxy-token") return send(response, 404, "text/plain", Buffer.from("disabled\n"));
  if (url.pathname === "/v2/_catalog") {
    if (url.searchParams.has("last")) {
      return sendJson(response, { repositories: ["demo/nested/one", "packages/datadog/agent"] });
    }
    return sendJson(
      response,
      { repositories: ["demo/image", "demo/nested/one"] },
      { Link: '</v2/_catalog?n=100&last=demo%2Fnested%2Fone>; rel="next"' },
    );
  }
  if (url.pathname === "/v2/demo/image/tags/list") {
    if (url.searchParams.has("last")) {
      return sendJson(response, { name: "demo/image", tags: ["multi", "overlay", "size-mismatch"] });
    }
    return sendJson(
      response,
      { name: "demo/image", tags: ["latest", "corrupt", "multi"] },
      { Link: '</v2/demo/image/tags/list?n=100&last=multi>; rel="next"' },
    );
  }
  if (url.pathname === "/v2/packages/datadog/agent/tags/list") {
    return sendJson(response, { name: "packages/datadog/agent", tags: ["latest", "7", "7.81", "7.81.1-1"] });
  }
  if (url.pathname.startsWith("/v2/demo/image/manifests/")) {
    const selector = decodeURIComponent(url.pathname.split("/").at(-1));
    if (selector === "multi") {
      return send(response, 200, "application/vnd.oci.image.index.v1+json", multiPlatformIndex);
    }
    if (selector === amdManifestDigest) {
      return send(response, 200, "application/vnd.oci.image.manifest.v1+json", amdManifest);
    }
    if (selector === armManifestDigest) {
      return send(response, 200, "application/vnd.oci.image.manifest.v1+json", armManifest);
    }
    if (selector === "overlay") {
      return send(response, 200, "application/vnd.oci.image.manifest.v1+json", overlayManifest);
    }
    return send(response, 200, "application/vnd.oci.image.manifest.v1+json", manifest(selector));
  }
  if (url.pathname.startsWith("/v2/packages/datadog/agent/manifests/")) {
    return send(response, 200, "application/vnd.oci.image.manifest.v1+json", packageManifest());
  }
  if (url.pathname === `/v2/demo/image/blobs/${configDigest}`) {
    return send(response, 200, "application/vnd.oci.image.config.v1+json", config);
  }
  if (url.pathname === `/v2/demo/image/blobs/${armConfigDigest}`) {
    return send(response, 200, "application/vnd.oci.image.config.v1+json", armConfig);
  }
  if (url.pathname === `/v2/demo/image/blobs/${overlayConfigDigest}`) {
    return send(response, 200, "application/vnd.oci.image.config.v1+json", overlayConfig);
  }
  if (url.pathname === `/v2/demo/image/blobs/${lowerLayerDigest}`) {
    return send(response, 200, "application/vnd.oci.image.layer.v1.tar", lowerLayer);
  }
  if (url.pathname === `/v2/demo/image/blobs/${upperLayerDigest}`) {
    return send(response, 200, "application/vnd.oci.image.layer.v1.tar", upperLayer);
  }
  if (url.pathname.startsWith("/v2/demo/image/blobs/")) {
    return streamLayer(response);
  }
  if (url.pathname === `/v2/packages/datadog/agent/blobs/${packageConfigDigest}`) {
    return send(response, 200, "application/vnd.datadog.package.v1", packageConfig);
  }
  if (url.pathname === `/v2/packages/datadog/agent/blobs/${installerDigest}`) {
    return send(response, 200, "application/vnd.datadog.package.installer.layer.v1", installer);
  }
  if (url.pathname === `/v2/packages/datadog/agent/blobs/${packageArchiveDigest}`) {
    return send(response, 200, "application/vnd.datadog.package.layer.v1.tar+gzip", packageArchive);
  }
  return serveStatic(url.pathname, response);
});

server.listen(port, "127.0.0.1", () => {
  process.stdout.write(`Playwright fixture server listening on http://127.0.0.1:${port}\n`);
});

for (const signal of ["SIGINT", "SIGTERM"]) {
  process.on(signal, () => server.close(() => process.exit(0)));
}

function manifest(selector) {
  const descriptor = {
    mediaType: "application/vnd.example.layer.v1.tar",
    digest: selector === "corrupt" ? corruptDigest : layerDigest,
    size: selector === "size-mismatch" ? layer.length + 1 : layer.length,
  };
  return imageManifest(configDigest, config.length, [descriptor]);
}

function imageManifest(selectedConfigDigest, selectedConfigSize, layers) {
  return json({
    schemaVersion: 2,
    mediaType: "application/vnd.oci.image.manifest.v1+json",
    config: {
      mediaType: "application/vnd.oci.image.config.v1+json",
      digest: selectedConfigDigest,
      size: selectedConfigSize,
    },
    layers,
  });
}

function packageManifest() {
  return json({
    schemaVersion: 2,
    mediaType: "application/vnd.oci.image.manifest.v1+json",
    config: { mediaType: "application/vnd.datadog.package.v1", digest: packageConfigDigest, size: packageConfig.length },
    layers: [
      { mediaType: "application/vnd.datadog.package.installer.layer.v1", digest: installerDigest, size: installer.length },
      { mediaType: "application/vnd.datadog.package.layer.v1.tar+gzip", digest: packageArchiveDigest, size: packageArchive.length },
    ],
  });
}

function streamLayer(response) {
  response.writeHead(200, {
    "Cache-Control": "no-store",
    "Content-Length": layer.length,
    "Content-Type": "application/vnd.example.layer.v1.tar",
  });
  let offset = 0;
  const write = () => {
    if (offset === layer.length) return response.end();
    const end = Math.min(offset + 8 * 1024, layer.length);
    response.write(layer.subarray(offset, end));
    offset = end;
    setTimeout(write, 10);
  };
  write();
}

async function serveStatic(pathname, response) {
  const relative = pathname === "/" ? "index.html" : pathname.replace(/^\/+/, "");
  const path = normalize(join(webRoot, relative));
  if (!path.startsWith(`${webRoot}/`) || !existsSync(path)) {
    return send(response, 404, "text/plain", Buffer.from("not found\n"));
  }
  const metadata = await stat(path);
  if (!metadata.isFile()) return send(response, 404, "text/plain", Buffer.from("not found\n"));
  response.writeHead(200, {
    "Cache-Control": "no-store",
    "Content-Length": metadata.size,
    "Content-Type": contentType(path),
    "X-Content-Type-Options": "nosniff",
  });
  createReadStream(path).pipe(response);
}

function sendJson(response, value, headers = {}) {
  send(response, 200, "application/json", json(value), headers);
}

function send(response, status, type, body, headers = {}) {
  response.writeHead(status, {
    "Cache-Control": "no-store",
    "Content-Length": body.length,
    "Content-Type": type,
    "X-Content-Type-Options": "nosniff",
    ...headers,
  });
  response.end(body);
}

function json(value) {
  return Buffer.from(JSON.stringify(value));
}

function digest(bytes) {
  return `sha256:${createHash("sha256").update(bytes).digest("hex")}`;
}

function tar(entries) {
  const blocks = [];
  for (const value of entries) {
    const entry = Array.isArray(value)
      ? { path: value[0], contents: value[1] }
      : value;
    const contents = entry.contents || Buffer.alloc(0);
    blocks.push(tarHeader(entry.path, contents.length, entry.type, entry.linkTarget), contents);
    const padding = (512 - (contents.length % 512)) % 512;
    if (padding) blocks.push(Buffer.alloc(padding));
  }
  blocks.push(Buffer.alloc(1024));
  return Buffer.concat(blocks);
}

function tarHeader(path, size, type = "0", linkTarget = "") {
  const header = Buffer.alloc(512);
  header.write(path, 0, 100, "utf8");
  writeOctal(header, 100, 8, 0o644);
  writeOctal(header, 108, 8, 0);
  writeOctal(header, 116, 8, 0);
  writeOctal(header, 124, 12, size);
  writeOctal(header, 136, 12, 0);
  header.fill(0x20, 148, 156);
  header[156] = type.charCodeAt(0);
  header.write(linkTarget, 157, 100, "utf8");
  header.write("ustar\0", 257, 6, "ascii");
  header.write("00", 263, 2, "ascii");
  const checksum = header.reduce((sum, byte) => sum + byte, 0);
  header.write(`${checksum.toString(8).padStart(6, "0")}\0 `, 148, 8, "ascii");
  return header;
}

function writeOctal(buffer, offset, length, value) {
  const encoded = `${value.toString(8).padStart(length - 1, "0")}\0`;
  buffer.write(encoded, offset, length, "ascii");
}

function contentType(path) {
  return ({
    ".css": "text/css; charset=utf-8",
    ".html": "text/html; charset=utf-8",
    ".js": "text/javascript; charset=utf-8",
    ".json": "application/json",
    ".wasm": "application/wasm",
  })[extname(path)] || "application/octet-stream";
}
