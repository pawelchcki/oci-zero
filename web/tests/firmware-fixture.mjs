// Builds firmware OCI layout tars for the flasher tests.
//
// The media types, annotation keys and config schema here must match
// tools/build-firmware-artifact.sh. `flash.spec.mjs` also runs against a tar
// produced by that script when OCI_ZERO_FIRMWARE_TAR is set, which is what keeps
// the two from drifting apart unnoticed.
import { createHash } from "node:crypto";
import { gzipSync } from "node:zlib";

export const ARTIFACT_TYPE = "application/vnd.oci-zero.firmware.v1+json";
export const CONFIG_MEDIA_TYPE = "application/vnd.oci-zero.firmware.config.v1+json";
export const LAYER_MEDIA_TYPE = "application/vnd.oci-zero.firmware.layer.v1.tar+gzip";
export const MANIFEST_MEDIA_TYPE = "application/vnd.oci.image.manifest.v1+json";
export const INDEX_MEDIA_TYPE = "application/vnd.oci.image.index.v1+json";

export const APP_OFFSET = 0x10000;
export const APP_DESC_OFFSET = 0x20;
export const APP_DESC_MAGIC = 0xabcd5432;

/**
 * A synthetic ESP32 application image: an 0x20-byte stand-in for
 * esp_image_header_t plus the first segment header, then an esp_app_desc_t whose
 * field offsets match esp-bootloader-esp-idf's `EspAppDesc`.
 */
export function appImage(version, { padTo = 4096 } = {}) {
  const image = Buffer.alloc(Math.max(padTo, APP_DESC_OFFSET + 256));
  image[0] = 0xe9; // ESP_IMAGE_HEADER_MAGIC, so the bytes look like an app image
  image[1] = 1; // one segment
  image.writeUInt32LE(APP_DESC_MAGIC, APP_DESC_OFFSET);
  image.writeUInt32LE(0, APP_DESC_OFFSET + 4); // secure_version
  // reserv1[2] occupies +8..+16, so `version` starts at +16.
  image.write(version, APP_DESC_OFFSET + 16, 32, "utf8");
  image.write("oci-zero-esp32c3-ota", APP_DESC_OFFSET + 48, 32, "utf8");
  return image;
}

/**
 * Assembles an OCI layout tar the flasher page can open.
 *
 * @param {object} options
 * @param {string} options.version       version annotation
 * @param {string} [options.revision]    revision annotation
 * @param {string} [options.chip]        chip annotation
 * @param {Array}  [options.entries]     `{ path, offset, data }` images
 * @param {false|"digest"|"payload"} [options.corruptLayer] corrupt the stored
 *                  layer blob after its descriptor was written.
 *                  `"digest"` flips a byte of the gzip MTIME field, which the
 *                  decoder ignores: the layer inflates and the tar parses, so the
 *                  only thing that can reject it is the descriptor digest check
 *                  at finish().
 *                  `"payload"` flips a byte mid-stream, which corrupts the
 *                  inflated tar and so fails earlier, in the archive parser.
 * @param {boolean} [options.pathPrefix]  emit `./name` members, as
 *                  `tar -C layout .` does, instead of bare `name`
 */
export function firmwareLayout(options) {
  const {
    version,
    revision = "0000000000000000000000000000000000000000",
    chip = "esp32c3",
    entries = [{ path: "firmware.bin", offset: APP_OFFSET, data: appImage(version) }],
    corruptLayer = false,
    pathPrefix = "./",
  } = options;

  const layerTar = tar(entries.map((entry) => [entry.path, entry.data]));
  const layer = gzipSync(layerTar, { level: 9 });
  const layerDigest = digest(layer);

  const config = json({
    chip,
    target: "riscv32imc-unknown-none-elf",
    entries: entries.map((entry) => ({ path: entry.path, offset: entry.offset })),
  });
  const configDigest = digest(config);

  const annotations = {
    "org.opencontainers.image.version": version,
    "org.opencontainers.image.revision": revision,
    "vnd.oci-zero.firmware.chip": chip,
  };
  const manifest = json({
    schemaVersion: 2,
    mediaType: MANIFEST_MEDIA_TYPE,
    artifactType: ARTIFACT_TYPE,
    config: { mediaType: CONFIG_MEDIA_TYPE, digest: configDigest, size: config.length },
    layers: [{ mediaType: LAYER_MEDIA_TYPE, digest: layerDigest, size: layer.length }],
    annotations,
  });
  const manifestDigest = digest(manifest);

  const index = json({
    schemaVersion: 2,
    mediaType: INDEX_MEDIA_TYPE,
    manifests: [{
      mediaType: MANIFEST_MEDIA_TYPE,
      artifactType: ARTIFACT_TYPE,
      digest: manifestDigest,
      size: manifest.length,
      annotations,
    }],
  });

  // Corrupt after the descriptor is sealed, which is exactly the situation the
  // digest check exists to catch.
  const storedLayer = Buffer.from(layer);
  if (corruptLayer === "digest") storedLayer[4] ^= 0xff; // gzip MTIME
  else if (corruptLayer === "payload") storedLayer[Math.floor(storedLayer.length / 2)] ^= 0xff;
  else if (corruptLayer) throw new Error(`unknown corruptLayer mode ${corruptLayer}`);

  const members = [
    ["oci-layout", Buffer.from('{"imageLayoutVersion":"1.0.0"}')],
    ["index.json", index],
    [`blobs/sha256/${hex(manifestDigest)}`, manifest],
    [`blobs/sha256/${hex(configDigest)}`, config],
    [`blobs/sha256/${hex(layerDigest)}`, storedLayer],
  ];

  return {
    tar: tar(members.map(([path, data]) => [`${pathPrefix}${path}`, data])),
    version,
    revision,
    chip,
    manifestDigest,
    configDigest,
    layerDigest,
    entries: entries.map((entry) => ({ ...entry, digest: digest(entry.data) })),
  };
}

function hex(value) {
  return value.slice("sha256:".length);
}

function json(value) {
  return Buffer.from(JSON.stringify(value));
}

function digest(bytes) {
  return `sha256:${createHash("sha256").update(bytes).digest("hex")}`;
}

function tar(entries) {
  const blocks = [];
  for (const [path, contents] of entries) {
    blocks.push(tarHeader(path, contents.length), contents);
    const padding = (512 - (contents.length % 512)) % 512;
    if (padding) blocks.push(Buffer.alloc(padding));
  }
  blocks.push(Buffer.alloc(1024));
  return Buffer.concat(blocks);
}

function tarHeader(path, size) {
  const header = Buffer.alloc(512);
  header.write(path, 0, 100, "utf8");
  writeOctal(header, 100, 8, 0o644);
  writeOctal(header, 108, 8, 0);
  writeOctal(header, 116, 8, 0);
  writeOctal(header, 124, 12, size);
  writeOctal(header, 136, 12, 0);
  header.fill(0x20, 148, 156); // checksum field counts as spaces while summing
  header[156] = "0".charCodeAt(0);
  header.write("ustar\0", 257, 6, "ascii");
  header.write("00", 263, 2, "ascii");
  const checksum = header.reduce((sum, byte) => sum + byte, 0);
  header.write(`${checksum.toString(8).padStart(6, "0")}\0 `, 148, 8, "ascii");
  return header;
}

function writeOctal(buffer, offset, length, value) {
  buffer.write(`${value.toString(8).padStart(length - 1, "0")}\0`, offset, length, "ascii");
}
