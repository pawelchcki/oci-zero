import { readFile } from "node:fs/promises";
import AxeBuilder from "@axe-core/playwright";
import { expect, test } from "@playwright/test";
import { APP_DESC_OFFSET, APP_OFFSET, appImage, firmwareLayout } from "./firmware-fixture.mjs";

const VERSION = "0.1.0-fixture+sha.abc1234";
const REVISION = "abc1234def5678000000000000000000000000ff";

const pageErrors = new WeakMap();

test.beforeEach(async ({ page }) => {
  const errors = [];
  pageErrors.set(page, errors);
  page.on("pageerror", (error) => errors.push(error.message));
  await installFakeEsptool(page);
  await page.goto("/flash.html");
  await expect(page.locator("#status")).toHaveText("Ready. Choose a firmware artifact.");
});

test.afterEach(async ({ page }) => {
  expect(pageErrors.get(page)).toEqual([]);
});

test("verifies a firmware artifact from index.json down to the flashable image", async ({ page }) => {
  const fixture = firmwareLayout({ version: VERSION, revision: REVISION });
  await open(page, fixture);

  await expect(page.locator("#status")).toHaveText(
    `Verified ${VERSION} for esp32c3: 1 image ready to flash.`,
  );
  await expect(page.locator("#artifact-version")).toHaveText(VERSION);
  await expect(page.locator("#artifact-revision")).toHaveText(REVISION);
  await expect(page.locator("#artifact-chip")).toHaveText("esp32c3");
  await expect(page.locator("#manifest-digest")).toHaveText(fixture.manifestDigest);
  await expect(page.locator("#config-digest")).toHaveText(fixture.configDigest);
  await expect(page.locator("#layer-digest")).toHaveText(fixture.layerDigest);

  // The digest the page shows for the extracted image has to be the digest of
  // the bytes the fixture put in the layer, or the extraction is not faithful.
  const row = page.locator(".file-row").filter({ hasText: "firmware.bin" });
  await expect(row).toContainText(fixture.entries[0].digest);
  await expect(row).toContainText(`0x${APP_OFFSET.toString(16)}`);
  await expect(page.locator("#manifest-json")).toContainText(
    "application/vnd.oci-zero.firmware.layer.v1.tar+gzip",
  );
  await expect(page.locator("#config-json")).toContainText('"offset": 65536');

  // The outer tar's digest is the page's own hash of the dropped file, so it has
  // to be presented as exactly that and nothing stronger.
  await expect(page.locator("#trust-note")).toContainText("it proves nothing on its own");
  await expect(page.locator("#flash")).toBeEnabled();
});

// The corrupted byte here is one the gzip decoder ignores, so the layer inflates
// and the tar parses cleanly. Nothing but oci-zero's descriptor digest check can
// reject it, which is what makes this the test of that check.
test("rejects a layer whose bytes no longer match its descriptor digest", async ({ page }) => {
  const fixture = firmwareLayout({ version: VERSION, corruptLayer: "digest" });
  await open(page, fixture);

  const status = page.locator("#status");
  await expect(status).toContainText("digest mismatch", { ignoreCase: true });
  await expect(status).toHaveClass(/error/);
  await expect(status).toHaveAttribute("role", "alert");
  await expect(page.locator("#artifact-panel")).toBeHidden();
  await expect(page.locator("#flash")).toBeDisabled();
});

test("rejects a layer whose compressed payload is damaged", async ({ page }) => {
  await open(page, firmwareLayout({ version: VERSION, corruptLayer: "payload" }));

  await expect(page.locator("#status")).toHaveClass(/error/);
  await expect(page.locator("#artifact-panel")).toBeHidden();
  await expect(page.locator("#flash")).toBeDisabled();
});

test("rejects an artifact whose manifest does not match the descriptor in index.json", async ({ page }) => {
  const fixture = firmwareLayout({ version: VERSION });
  // Rename the manifest blob to a digest it does not have: index.json still
  // points at that name, so the manifest arrives unverified.
  const tampered = replaceInTar(fixture.tar, fixture.manifestDigest, (manifest) => {
    const patched = Buffer.from(manifest);
    patched.write("9", patched.indexOf('"schemaVersion":2') + 16, 1, "ascii");
    return patched;
  });
  await open(page, { ...fixture, tar: tampered });

  await expect(page.locator("#status")).toContainText("manifest digest mismatch");
  await expect(page.locator("#artifact-panel")).toBeHidden();
});

test("reads members named without a ./ prefix", async ({ page }) => {
  const fixture = firmwareLayout({ version: VERSION, pathPrefix: "" });
  await open(page, fixture);
  await expect(page.locator("#artifact-version")).toHaveText(VERSION);
});

test("writes every image at its declared offset and resets the board", async ({ page }) => {
  const fixture = firmwareLayout({
    version: VERSION,
    entries: [
      { path: "bootloader.bin", offset: 0x0, data: Buffer.alloc(2048, 0x5a) },
      { path: "firmware.bin", offset: APP_OFFSET, data: appImage(VERSION) },
    ],
  });
  await open(page, fixture);
  await expect(page.locator("#status")).toContainText("2 images ready to flash");

  await page.locator("#flash").click();
  await expect(page.locator("#status")).toHaveText(`Flashed ${VERSION}. Resetting the board.`);
  await expect(page.locator("#progress")).toHaveJSProperty("value", 100);

  const written = await page.evaluate(() => globalThis.ociZeroEsptoolCalls);
  expect(written.writeFlash).toHaveLength(1);
  expect(written.writeFlash[0].addresses).toEqual([0x0, APP_OFFSET]);
  expect(written.writeFlash[0].sizes).toEqual([2048, fixture.entries[1].data.length]);
  // "keep" everywhere: the header espflash produced already carries the right
  // mode, frequency and size.
  expect(written.writeFlash[0].flashMode).toBe("keep");
  expect(written.writeFlash[0].flashSize).toBe("keep");
  // On by default: the artifact replaces the partition table, so anything the
  // board's old layout left at the new otadata or nvs offsets has to go.
  expect(written.writeFlash[0].eraseAll).toBe(true);
  expect(written.after).toEqual(["hard_reset"]);
  expect(written.disconnects).toBe(1);
});

test("keeps the flash when the erase checkbox is cleared", async ({ page }) => {
  const fixture = firmwareLayout({ version: VERSION });
  await open(page, fixture);

  await page.locator("#erase-all").uncheck();
  await page.locator("#flash").click();
  await expect(page.locator("#status")).toHaveText(`Flashed ${VERSION}. Resetting the board.`);

  const written = await page.evaluate(() => globalThis.ociZeroEsptoolCalls);
  expect(written.writeFlash[0].eraseAll).toBe(false);
});

test("refuses to flash an artifact built for a different chip", async ({ page }) => {
  const fixture = firmwareLayout({ version: VERSION, chip: "esp32s3" });
  await open(page, fixture);
  await page.locator("#flash").click();

  await expect(page.locator("#status")).toContainText("the artifact targets esp32s3 but the board is ESP32-C3");
  const written = await page.evaluate(() => globalThis.ociZeroEsptoolCalls);
  expect(written.writeFlash).toEqual([]);
  // Refusing still has to hand the serial port back.
  expect(written.disconnects).toBe(1);
});

test("reads the installed version out of the application descriptor", async ({ page }) => {
  const fixture = firmwareLayout({ version: VERSION });
  await open(page, fixture);
  await page.evaluate((image) => {
    globalThis.ociZeroFlashContents = new Uint8Array(image);
  }, [...appImage("9.9.9-installed")]);

  await page.locator("#read-version").click();
  await expect(page.locator("#installed-version")).toHaveText("Installed: 9.9.9-installed");
  await expect(page.locator("#status")).toHaveText("The board reports version 9.9.9-installed.");

  const read = await page.evaluate(() => globalThis.ociZeroEsptoolCalls.readFlash);
  expect(read).toEqual([{ address: APP_OFFSET, size: APP_DESC_OFFSET + 256 }]);
});

test("reports an empty slot instead of showing a garbage version", async ({ page }) => {
  await page.evaluate(() => {
    globalThis.ociZeroFlashContents = new Uint8Array(0x120).fill(0xff);
  });
  await page.locator("#read-version").click();
  await expect(page.locator("#status")).toContainText("no application descriptor");
  await expect(page.locator("#status")).toHaveClass(/error/);
});

test("runs the self-contained flash build without external code assets", async ({ page }) => {
  await page.goto("/dist/flash.html");
  await expect(page.locator("#status")).toHaveText("Ready. Choose a firmware artifact.");
  const codeAssets = await page.evaluate(() => performance
    .getEntriesByType("resource")
    .map((entry) => entry.name)
    .filter((url) => /\.(?:css|js|wasm)(?:$|\?)/.test(url)));
  expect(codeAssets).toEqual([]);

  const fixture = firmwareLayout({ version: VERSION });
  await open(page, fixture);
  await expect(page.locator("#artifact-version")).toHaveText(VERSION);
  await expect(page.locator("#layer-digest")).toHaveText(fixture.layerDigest);
});

test("has no detectable accessibility violations once an artifact is loaded", async ({ page }) => {
  await open(page, firmwareLayout({ version: VERSION }));
  const results = await new AxeBuilder({ page }).analyze();
  expect(results.violations).toEqual([]);
});

// Runs against a tar produced by tools/build-firmware-artifact.sh, so the real
// script and the JS fixture cannot drift apart without something failing. CI
// sets the variable; locally the test is skipped.
test("opens a layout tar built by tools/build-firmware-artifact.sh", async ({ page }) => {
  const path = process.env.OCI_ZERO_FIRMWARE_TAR;
  test.skip(!path, "set OCI_ZERO_FIRMWARE_TAR to a layout tar to run this");

  await page.setInputFiles("#artifact-input", {
    name: "firmware.oci.tar",
    mimeType: "application/x-tar",
    buffer: await readFile(path),
  });
  await expect(page.locator("#status")).toContainText("ready to flash");
  await expect(page.locator("#artifact-version")).toHaveText(
    process.env.OCI_ZERO_FIRMWARE_VERSION || /.+/,
  );
  await expect(page.locator("#manifest-digest")).toHaveText(/^sha256:[0-9a-f]{64}$/);
});

async function open(page, fixture) {
  await page.setInputFiles("#artifact-input", {
    name: `firmware-${fixture.version}.oci.tar`,
    mimeType: "application/x-tar",
    buffer: fixture.tar,
  });
}

// Replaces the contents of the tar member holding `digest` with `patch(bytes)`,
// leaving the member's name — and therefore index.json's reference — untouched.
function replaceInTar(archive, digest, patch) {
  const name = `blobs/sha256/${digest.slice("sha256:".length)}`;
  for (let offset = 0; offset + 512 <= archive.length; offset += 512) {
    const header = archive.subarray(offset, offset + 512);
    const path = header.subarray(0, 100).toString("utf8").replace(/\0.*$/, "");
    const size = Number.parseInt(header.subarray(124, 136).toString("ascii").replace(/\0.*$/, "").trim(), 8);
    if (!path) break;
    const body = offset + 512;
    if (path.endsWith(name)) {
      const replacement = patch(archive.subarray(body, body + size));
      if (replacement.length !== size) throw new Error("patch must preserve the member size");
      const copy = Buffer.from(archive);
      replacement.copy(copy, body);
      return copy;
    }
    offset = body + Math.ceil(size / 512) * 512 - 512;
  }
  throw new Error(`${name} is not in the fixture tar`);
}

// A stand-in for esptool-js. The page's flash and readback paths run for real;
// the ROM bootloader protocol below them does not, which is the deliberate
// boundary — emulating it would be testing esptool-js, not this page.
async function installFakeEsptool(page) {
  await page.addInitScript(() => {
    const calls = { writeFlash: [], readFlash: [], after: [], disconnects: 0 };
    globalThis.ociZeroEsptoolCalls = calls;
    globalThis.ociZeroFlashContents = new Uint8Array(0x120).fill(0xff);

    // defineProperty rather than assignment: `navigator.serial` is a read-only
    // accessor when the browser does support Web Serial, so a plain assignment
    // would be silently dropped.
    Object.defineProperty(navigator, "serial", {
      configurable: true,
      value: { requestPort: async () => ({ fake: true }) },
    });

    class FakeTransport {
      constructor(device) {
        this.device = device;
      }

      async disconnect() {
        calls.disconnects += 1;
      }
    }

    class FakeESPLoader {
      constructor(options) {
        this.options = options;
        this.chip = { CHIP_NAME: "ESP32-C3" };
      }

      async main() {
        this.options.terminal?.writeLine("Chip is ESP32-C3 (fake)");
        return "ESP32-C3";
      }

      async writeFlash(options) {
        calls.writeFlash.push({
          addresses: options.fileArray.map((file) => file.address),
          sizes: options.fileArray.map((file) => file.data.length),
          flashMode: options.flashMode,
          flashFreq: options.flashFreq,
          flashSize: options.flashSize,
          eraseAll: options.eraseAll,
          compress: options.compress,
        });
        for (const [index, file] of options.fileArray.entries()) {
          options.reportProgress?.(index, file.data.length >> 1, file.data.length);
          options.reportProgress?.(index, file.data.length, file.data.length);
        }
      }

      async readFlash(address, size) {
        calls.readFlash.push({ address, size });
        const contents = globalThis.ociZeroFlashContents;
        const out = new Uint8Array(size);
        out.set(contents.subarray(0, Math.min(size, contents.length)));
        return out;
      }

      async after(mode) {
        calls.after.push(mode);
      }
    }

    globalThis.ociZeroEsptool = { ESPLoader: FakeESPLoader, Transport: FakeTransport };
  });
}
