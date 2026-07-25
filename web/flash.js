import init, { extract_file, parse_document, sha256 } from "./pkg/oci_zero_web.js?v=20260725-1";
// The bundled build (build-flash.mjs) rewrites this import into inlined source,
// so keep it a single bare specifier on one line.
import { ESPLoader, Transport } from "./node_modules/esptool-js/bundle.js";

// `.tar` is what selects `Encoding::Tar` in web/src/lib.rs's `browser_encoding`,
// which is why an otherwise unused media type is spelled out here.
const LAYOUT_MEDIA_TYPE = "application/vnd.oci.image.layout.v1.tar";
const ARTIFACT_TYPE = "application/vnd.oci-zero.firmware.v1+json";
const CONFIG_MEDIA_TYPE = "application/vnd.oci-zero.firmware.config.v1+json";
const VERSION_ANNOTATION = "org.opencontainers.image.version";
const REVISION_ANNOTATION = "org.opencontainers.image.revision";
const CHIP_ANNOTATION = "vnd.oci-zero.firmware.chip";
const APP_ENTRY = "firmware.bin";

// An OCI layout tar names its members relative to the layout root, but whether
// `tar` was told `-C layout .` or `-C layout index.json blobs` decides whether
// that arrives as `./index.json` or `index.json`. oci-zero matches the archive's
// own path, so both spellings are tried and the winner is reused.
const PATH_PREFIXES = ["", "./"];

// esp_app_desc_t, confirmed against esp-bootloader-esp-idf's `EspAppDesc`:
// it is the start of the first flash segment, so it follows the 24-byte
// esp_image_header_t plus the 8-byte esp_image_segment_header_t, and `version`
// follows magic_word, secure_version and reserv1[2].
const APP_DESC_OFFSET = 0x20;
const APP_DESC_SIZE = 256;
const APP_DESC_MAGIC = 0xabcd5432;
const APP_DESC_VERSION_OFFSET = 16;
const APP_DESC_VERSION_SIZE = 32;

const FLASH_BAUDRATE = 921_600;
const ROM_BAUDRATE = 115_200;
const LOG_LIMIT = 200;
const MAX_ARTIFACT_BYTES = 64 * 1024 * 1024;

const $ = (id) => document.getElementById(id);
const state = { artifact: null, busy: false };

await init({ module_or_path: new URL("./pkg/oci_zero_web_bg.wasm?v=20260725-1", import.meta.url) });
initializeUi();

function initializeUi() {
  $("runtime-badge").classList.add("hidden");
  $("artifact-input").addEventListener("change", (event) => {
    const [file] = event.target.files;
    if (file) guard(() => openArtifact(file));
  });
  $("flash").addEventListener("click", () => guard(flashArtifact));
  $("read-version").addEventListener("click", () => guard(readInstalledVersion));
  $("log-clear").addEventListener("click", () => $("log-entries").replaceChildren());

  const supported = Boolean(navigator.serial);
  $("serial-support").textContent = supported
    ? "Web Serial is available. Connect the board over USB and hold BOOT if it does not answer."
    : "This browser has no Web Serial API. Use Chrome, Edge or Opera on desktop; Firefox and Safari cannot flash.";
  $("read-version").disabled = !supported;
  setStatus("Ready. Choose a firmware artifact.");
}

// Every entry point funnels through here so a rejected promise becomes a visible
// error instead of an unhandled rejection, and so two operations cannot overlap
// on one serial port.
async function guard(action) {
  if (state.busy) {
    setStatus("Another operation is still running.", true);
    return;
  }
  state.busy = true;
  try {
    await action();
  } catch (error) {
    setStatus(error?.message || String(error), true);
  } finally {
    state.busy = false;
  }
}

async function openArtifact(file) {
  setStatus(`Reading ${file.name} (${formatBytes(file.size)}).`);
  if (file.size > MAX_ARTIFACT_BYTES) {
    throw new Error(`${file.name} is larger than the ${formatBytes(MAX_ARTIFACT_BYTES)} limit`);
  }
  const bytes = new Uint8Array(await file.arrayBuffer());
  const artifact = readLayout(bytes);
  state.artifact = artifact;
  renderArtifact(file, artifact);
  setStatus(
    `Verified ${artifact.version} for ${artifact.chip}: ` +
    `${artifact.entries.length} image${artifact.entries.length === 1 ? "" : "s"} ready to flash.`,
  );
}

// Walks index.json -> manifest -> config -> layer -> images, checking each blob
// against the descriptor that referenced it.
function readLayout(bytes) {
  const outerDigest = sha256(bytes);
  const readBlob = layoutReader(bytes, outerDigest);

  const index = parse_document(readBlob("index.json"));
  if (index.kind !== "index") throw new Error("index.json is not an OCI image index");
  if (index.manifests.length !== 1) {
    throw new Error(`expected exactly one manifest in index.json, found ${index.manifests.length}`);
  }
  const manifestDescriptor = index.manifests[0];
  const manifestBytes = readDescriptor(readBlob, manifestDescriptor, "manifest");

  const manifest = parse_document(manifestBytes);
  if (manifest.kind !== "manifest") throw new Error("the referenced blob is not an OCI manifest");
  const artifactType = manifest.artifact_type ?? manifestDescriptor.artifact_type;
  if (artifactType !== ARTIFACT_TYPE) {
    throw new Error(`unexpected artifactType ${artifactType ?? "(none)"}; expected ${ARTIFACT_TYPE}`);
  }

  const annotations = new Map(manifest.annotations);
  const version = annotations.get(VERSION_ANNOTATION);
  if (!version) throw new Error(`the manifest has no ${VERSION_ANNOTATION} annotation`);

  const configDescriptor = manifest.config;
  if (configDescriptor.media_type !== CONFIG_MEDIA_TYPE) {
    throw new Error(`unexpected config media type ${configDescriptor.media_type}`);
  }
  const configBytes = readDescriptor(readBlob, configDescriptor, "config");
  const config = JSON.parse(new TextDecoder().decode(configBytes));
  if (!Array.isArray(config.entries) || config.entries.length === 0) {
    throw new Error("the flash config lists no entries");
  }

  if (manifest.layers.length !== 1) {
    throw new Error(`expected exactly one layer, found ${manifest.layers.length}`);
  }
  const layerDescriptor = manifest.layers[0];
  // Deliberately *not* pre-hashed like the manifest and config are. Those two
  // are handed to the JSON parsers directly, so nothing else would check them;
  // the layer instead goes through VerifiedDecoder below, which hashes it while
  // inflating and rejects a wrong digest or length at finish(). Pre-hashing here
  // would only duplicate that and hide which check is load-bearing.
  const layerBytes = readBlob(`blobs/sha256/${expectSha256(layerDescriptor.digest, "layer")}`);

  const entries = config.entries.map((entry) => {
    if (typeof entry.path !== "string" || !Number.isSafeInteger(entry.offset) || entry.offset < 0) {
      throw new Error(`invalid flash entry ${JSON.stringify(entry)}`);
    }
    // The digest and size come from the manifest, not from the archive, so this
    // is the call that actually verifies the payload: VerifiedDecoder in
    // compressed-only mode hashes the layer while inflating it and fails at
    // finish() if the bytes do not match.
    const data = extract_file(
      layerBytes,
      layerDescriptor.media_type,
      layerDescriptor.digest,
      BigInt(layerDescriptor.size),
      null,
      entry.path,
    );
    return { path: entry.path, offset: entry.offset, data, digest: sha256(data) };
  });

  return {
    outerDigest,
    manifestDigest: manifestDescriptor.digest,
    configDigest: configDescriptor.digest,
    layerDigest: layerDescriptor.digest,
    version,
    revision: annotations.get(REVISION_ANNOTATION) || "(none)",
    chip: annotations.get(CHIP_ANNOTATION) || config.chip || "(unknown)",
    manifestJson: new TextDecoder().decode(manifestBytes),
    configJson: new TextDecoder().decode(configBytes),
    entries,
  };
}

function layoutReader(bytes, digest) {
  const size = BigInt(bytes.length);
  let prefix = null;
  return (path) => {
    const candidates = prefix === null ? PATH_PREFIXES : [prefix];
    let failure;
    for (const candidate of candidates) {
      try {
        const blob = extract_file(bytes, LAYOUT_MEDIA_TYPE, digest, size, null, `${candidate}${path}`);
        prefix = candidate;
        return blob;
      } catch (error) {
        failure = error;
      }
    }
    throw new Error(`${path} is not in the layout tar: ${failure?.message || failure}`);
  };
}

function readDescriptor(readBlob, descriptor, label) {
  const bytes = readBlob(`blobs/sha256/${expectSha256(descriptor.digest, label)}`);
  const expected = Number(descriptor.size);
  if (bytes.length !== expected) {
    throw new Error(`${label} blob is ${bytes.length} bytes, descriptor says ${expected}`);
  }
  const actual = sha256(bytes);
  if (actual !== descriptor.digest) {
    throw new Error(`${label} digest mismatch: descriptor ${descriptor.digest}, archive ${actual}`);
  }
  return bytes;
}

function expectSha256(digest, label) {
  const match = /^sha256:([0-9a-f]{64})$/.exec(digest ?? "");
  if (!match) throw new Error(`${label} descriptor has an unsupported digest ${digest}`);
  return match[1];
}

function renderArtifact(file, artifact) {
  $("artifact-panel").classList.remove("hidden");
  $("artifact-name").textContent = file.name;
  $("artifact-chip").textContent = artifact.chip;
  $("artifact-version").textContent = artifact.version;
  $("artifact-revision").textContent = artifact.revision;
  $("manifest-digest").textContent = artifact.manifestDigest;
  $("config-digest").textContent = artifact.configDigest;
  $("layer-digest").textContent = artifact.layerDigest;
  $("outer-digest").textContent = artifact.outerDigest;
  $("manifest-json").textContent = prettyJson(artifact.manifestJson);
  $("config-json").textContent = prettyJson(artifact.configJson);

  const body = $("entry-results-body");
  body.replaceChildren();
  for (const entry of artifact.entries) {
    const row = document.createElement("tr");
    row.className = "file-row";

    const path = document.createElement("th");
    path.scope = "row";
    path.append(code(entry.path));
    row.append(path);
    row.append(cell("Offset", `0x${entry.offset.toString(16)}`));
    row.append(cell("Size", formatBytes(entry.data.length)));
    row.append(cell("Digest", entry.digest));
    body.append(row);
  }

  $("flash").disabled = !navigator.serial;
}

async function flashArtifact() {
  const artifact = state.artifact;
  if (!artifact) throw new Error("choose a firmware artifact first");
  const { loader, transport } = await connect();
  try {
    warnOnChipMismatch(loader, artifact);
    const total = artifact.entries.reduce((sum, entry) => sum + entry.data.length, 0);
    setProgress(0);
    await loader.writeFlash({
      fileArray: artifact.entries.map((entry) => ({ data: entry.data, address: entry.offset })),
      // `keep` everywhere: the images were produced by espflash for this chip,
      // so re-deriving flash mode, frequency or size here could only disagree
      // with the header that is already correct.
      flashMode: "keep",
      flashFreq: "keep",
      flashSize: "keep",
      eraseAll: false,
      compress: true,
      reportProgress: (fileIndex, written) => {
        const before = artifact.entries
          .slice(0, fileIndex)
          .reduce((sum, entry) => sum + entry.data.length, 0);
        setProgress(((before + written) / total) * 100);
      },
    });
    setProgress(100);
    setStatus(`Flashed ${artifact.version}. Resetting the board.`);
    await loader.after("hard_reset");
  } finally {
    await release(transport);
  }
}

async function readInstalledVersion() {
  const offset = state.artifact?.entries.find((entry) => entry.path === APP_ENTRY)?.offset ?? 0x10000;
  const { loader, transport } = await connect();
  try {
    setStatus(`Reading the application descriptor at 0x${offset.toString(16)}.`);
    const raw = await loader.readFlash(offset, APP_DESC_OFFSET + APP_DESC_SIZE);
    const version = parseAppDescriptorVersion(raw);
    $("installed-version").textContent = `Installed: ${version}`;
    setStatus(`The board reports version ${version}.`);
  } finally {
    await release(transport);
  }
}

// Parses esp_app_desc_t out of the first bytes of an application partition.
function parseAppDescriptorVersion(raw) {
  if (raw.length < APP_DESC_OFFSET + APP_DESC_VERSION_OFFSET + APP_DESC_VERSION_SIZE) {
    throw new Error(`read only ${raw.length} bytes; not enough for an application descriptor`);
  }
  const view = new DataView(raw.buffer, raw.byteOffset, raw.byteLength);
  const magic = view.getUint32(APP_DESC_OFFSET, true);
  if (magic !== APP_DESC_MAGIC) {
    throw new Error(
      `no application descriptor at +0x${APP_DESC_OFFSET.toString(16)}: ` +
      `magic 0x${magic.toString(16).padStart(8, "0")}, expected 0x${APP_DESC_MAGIC.toString(16)}. ` +
      "The slot is probably empty or holds an image without esp_app_desc!().",
    );
  }
  const start = APP_DESC_OFFSET + APP_DESC_VERSION_OFFSET;
  const field = raw.subarray(start, start + APP_DESC_VERSION_SIZE);
  const end = field.indexOf(0);
  const version = new TextDecoder().decode(end === -1 ? field : field.subarray(0, end));
  if (!version) throw new Error("the application descriptor carries an empty version string");
  return version;
}

async function connect() {
  if (!navigator.serial) throw new Error("this browser has no Web Serial API");
  // Test seam. The end-to-end suite has no ESP32 attached and emulating the ROM
  // bootloader protocol byte for byte would test esptool-js rather than this
  // page, so tests substitute the two constructors and assert on what the page
  // asks them to write. Nothing sets this in a browser.
  const { ESPLoader: Loader, Transport: Port } = globalThis.ociZeroEsptool ?? { ESPLoader, Transport };
  setStatus("Waiting for a serial port to be chosen.");
  const port = await navigator.serial.requestPort();
  const transport = new Port(port, false);
  const loader = new Loader({
    transport,
    baudrate: FLASH_BAUDRATE,
    romBaudrate: ROM_BAUDRATE,
    terminal: {
      clean: () => {},
      write: (data) => appendLogEntry(data.trimEnd(), false),
      writeLine: (data) => appendLogEntry(data.trimEnd(), false),
    },
  });
  setStatus("Connecting to the bootloader.");
  const chip = await loader.main();
  setStatus(`Connected to ${chip}.`);
  return { loader, transport };
}

function warnOnChipMismatch(loader, artifact) {
  const detected = loader.chip?.CHIP_NAME;
  if (!detected) return;
  const normalize = (value) => value.toLowerCase().replace(/[^a-z0-9]/g, "");
  if (normalize(detected) !== normalize(artifact.chip)) {
    throw new Error(
      `the artifact targets ${artifact.chip} but the board is ${detected}; refusing to flash`,
    );
  }
}

async function release(transport) {
  try {
    await transport.disconnect();
  } catch (error) {
    appendLogEntry(`Could not close the serial port: ${error?.message || error}`, true);
  }
}

function setProgress(percent) {
  $("progress").value = Math.max(0, Math.min(100, percent));
}

function prettyJson(text) {
  try {
    return JSON.stringify(JSON.parse(text), null, 2);
  } catch {
    return text;
  }
}

function code(value) {
  const element = document.createElement("code");
  element.textContent = value;
  return element;
}

function cell(label, value) {
  const element = document.createElement("td");
  element.dataset.label = label;
  element.append(code(value));
  return element;
}

function formatBytes(value) {
  const bytes = Number(value);
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 ** 2) return `${(bytes / 1024).toFixed(1)} KiB`;
  if (bytes < 1024 ** 3) return `${(bytes / 1024 ** 2).toFixed(1)} MiB`;
  return `${(bytes / 1024 ** 3).toFixed(1)} GiB`;
}

function setStatus(message, error = false) {
  const status = $("status");
  const text = error && !/^Error:\s/i.test(message) ? `Error: ${message}` : message;
  status.textContent = text;
  status.classList.toggle("error", error);
  status.setAttribute("role", error ? "alert" : "status");
  status.setAttribute("aria-live", error ? "assertive" : "polite");
  appendLogEntry(text, error);
}

function appendLogEntry(message, error) {
  const list = $("log-entries");
  if (!list || !message) return;
  const last = list.lastElementChild;
  if (last && last.dataset.message === message) return;

  const entry = document.createElement("li");
  entry.className = error ? "log-entry error" : "log-entry";
  entry.dataset.message = message;

  const time = document.createElement("time");
  const now = new Date();
  time.dateTime = now.toISOString();
  time.textContent = now.toLocaleTimeString();

  const body = document.createElement("span");
  body.textContent = message;

  entry.append(time, body);
  list.append(entry);
  while (list.childElementCount > LOG_LIMIT) list.removeChild(list.firstElementChild);
  list.scrollTop = list.scrollHeight;
}
