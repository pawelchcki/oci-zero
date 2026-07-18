import { readFile } from "node:fs/promises";
import AxeBuilder from "@axe-core/playwright";
import { expect, test } from "@playwright/test";

const pageErrors = new WeakMap();

test.beforeEach(async ({ page }) => {
  const errors = [];
  pageErrors.set(page, errors);
  page.on("pageerror", (error) => errors.push(error.message));
  await page.addInitScript(() => {
    Object.defineProperty(globalThis, "showSaveFilePicker", { value: undefined });
  });
  await page.goto("/");
  await expect(page.locator("#runtime-badge")).toHaveText("Web / CORS mode");
});

test.afterEach(async ({ page }) => {
  expect(pageErrors.get(page)).toEqual([]);
});

test("browses a catalog, repository, and image manifest", async ({ page, baseURL }) => {
  const status = page.locator("#status");
  await expect(status).toHaveAttribute("role", "status");
  await expect(status).toHaveAttribute("aria-live", "polite");
  await expect(status).toHaveAttribute("aria-atomic", "true");

  await page.locator("#registry-input").fill(baseURL);
  await page.getByRole("button", { name: "Open catalog" }).click();
  await expect(status).toHaveText(`Loaded 2 repositories from ${new URL(baseURL).host}.`);
  await page.locator("#catalog-results summary", { hasText: "demo" }).click();
  await page.getByRole("button", { name: "Open repository demo/image" }).click();

  await expect(page.locator("#repository-name")).toHaveText(`${new URL(baseURL).host}/demo/image`);
  const group = page.locator(".version-group").filter({ hasText: "latest" });
  await group.getByRole("button", { name: "Open version latest" }).click();

  await expect(page.locator("#selection-name")).toContainText("demo/image:latest");
  await expect(status).toHaveText("Ready: 1 verified image layer.");
  await expect(status).toHaveAttribute("role", "status");
  await expect(group).toHaveAttribute("aria-current", "true");
  await expect(page.locator("#scan-files")).toBeEnabled();
  await expect(page.locator("#manifest-json")).toContainText("application/vnd.example.layer.v1.tar");
});

test("streams file entries in a native table, filters them, and downloads one", async ({ page, baseURL }) => {
  await openImage(page, baseURL, "latest");
  await page.locator("#scan-files").click();

  const table = page.getByRole("table", { name: "Merged filesystem entries" });
  const helloRow = table.getByRole("row").filter({ hasText: "etc/hello.txt" });
  await expect(helloRow).toBeVisible();
  await expect(page.locator("#status")).toContainText("Scanning layer");
  await expect(page.locator("#layer-results")).toContainText("scanning");
  await expect(helloRow.getByRole("button", { name: "Scanning etc/hello.txt" })).toBeDisabled();

  await expect(page.locator("#status")).toHaveText("Scanned 1 layer.", { timeout: 10_000 });
  await expect(page.locator("#layer-results")).toContainText("verified · 2 entries");
  await expect(helloRow.getByRole("button", { name: "Download etc/hello.txt" })).toBeEnabled();
  await expect(table.getByRole("columnheader")).toHaveText(["Path", "Type", "Size", "Source", "Status", "Action"]);
  await expect(table.getByRole("rowheader", { name: "etc/hello.txt" })).toBeVisible();

  await page.locator("#file-filter").fill("hello");
  await expect(page.locator(".file-row")).toHaveCount(1);
  await expect(page.locator("#file-limit")).toHaveText("1 matching entry.");

  const downloadPromise = page.waitForEvent("download");
  await helloRow.getByRole("button", { name: "Download etc/hello.txt" }).click();
  const download = await downloadPromise;
  expect(download.suggestedFilename()).toBe("hello.txt");
  expect(await readFile(await download.path(), "utf8")).toBe("hello from the streamed layer\n");
  await expect(page.locator("#status")).toContainText("Downloaded etc/hello.txt");
});

test("rolls back provisional entries when layer digest verification fails", async ({ page, baseURL }) => {
  await openImage(page, baseURL, "corrupt");
  await page.locator("#scan-files").click();

  await expect(page.locator(".file-row").filter({ hasText: "etc/hello.txt" })).toBeVisible();
  const status = page.locator("#status");
  await expect(status).toContainText("Error: digest mismatch", { ignoreCase: true, timeout: 10_000 });
  await expect(status).toHaveClass(/error/);
  await expect(status).toHaveAttribute("role", "alert");
  await expect(status).toHaveAttribute("aria-live", "assertive");
  await expect(page.locator(".file-row")).toHaveCount(0);
  await expect(page.locator("#layer-results")).toContainText("failed · 0 entries");
  await expect(page.locator("#scan-files")).toBeEnabled();
});

test("groups package aliases and scans supported artifact layers after skipped payloads", async ({ page, baseURL }) => {
  await page.locator("#repository-input").fill(`${baseURL}/packages/datadog/agent`);
  await page.getByRole("button", { name: "Browse tags" }).click();
  const group = page.locator(".version-group").filter({ hasText: "7.81.1-1" });
  await expect(group).toContainText("latest");
  await expect(group).toContainText("sha256:");
  await group.getByRole("button", { name: "Open alias latest" }).click();
  await expect(group).toHaveAttribute("aria-current", "true");
  await expect(page.locator("#selection-name")).toContainText(":latest");
  await expect(page.locator("#status")).toContainText("compressed descriptor verification");
  await expect(page.locator("#export-docker")).toBeDisabled();

  const installerDownloadPromise = page.waitForEvent("download");
  await page.getByRole("button", { name: "Download executable for layer 1" }).click();
  const installerDownload = await installerDownloadPromise;
  expect(installerDownload.suggestedFilename()).toBe("datadog-installer");
  expect(await readFile(await installerDownload.path(), "utf8")).toBe("unsupported package installer");
  await expect(page.locator("#status")).toContainText("Downloaded verified datadog-installer");

  await page.locator("#scan-files").click();
  await expect(page.locator("#status")).toHaveText("Scanned 1 layer; skipped 1 unsupported layer.", { timeout: 10_000 });
  const row = page.locator(".file-row").filter({ hasText: "application_monitoring.yaml.example" });
  await expect(row).toContainText("verified");
  const downloadPromise = page.waitForEvent("download");
  await row.getByRole("button", { name: "Download etc/datadog-agent/application_monitoring.yaml.example" }).click();
  const download = await downloadPromise;
  expect(await readFile(await download.path(), "utf8")).toContain("Datadog APM");
});

test("runs the self-contained proxyless build without external code assets", async ({ page, baseURL }) => {
  await page.goto("/dist/proxyless.html");
  await expect(page.locator("#runtime-badge")).toHaveText("Web / CORS mode");
  const codeAssets = await page.evaluate(() => performance
    .getEntriesByType("resource")
    .map((entry) => entry.name)
    .filter((url) => /\.(?:css|js|wasm)(?:$|\?)/.test(url)));
  expect(codeAssets).toEqual([]);

  await openImage(page, baseURL, "latest");
  await page.locator("#scan-files").click();
  await expect(page.locator("#status")).toHaveText("Scanned 1 layer.", { timeout: 10_000 });
  await expect(page.locator(".file-row").filter({ hasText: "etc/hello.txt" })).toBeVisible();
});

test("keeps populated results usable within laptop and phone viewports", async ({ page, baseURL }) => {
  for (const viewport of [{ width: 1024, height: 768 }, { width: 320, height: 568 }]) {
    await page.setViewportSize(viewport);
    await page.goto("/");
    await openImage(page, baseURL, "latest");
    await page.locator("#scan-files").click();
    await expect(page.locator("#status")).toHaveText("Scanned 1 layer.", { timeout: 10_000 });

    const horizontalOverflow = await page.evaluate(() =>
      document.documentElement.scrollWidth - document.documentElement.clientWidth);
    expect(horizontalOverflow).toBe(0);
    const download = page.getByRole("button", { name: "Download etc/hello.txt" });
    await download.scrollIntoViewIfNeeded();
    await expect(download).toBeInViewport();
    const dimensions = await download.evaluate((element) => {
      const { width, height } = element.getBoundingClientRect();
      return { width, height };
    });
    if (viewport.width <= 500) {
      expect(dimensions.width).toBeGreaterThanOrEqual(44);
      expect(dimensions.height).toBeGreaterThanOrEqual(44);
      for (const selector of ["#repository-input", "#scan-files", "#file-filter"]) {
        const box = await page.locator(selector).boundingBox();
        expect(box.height).toBeGreaterThanOrEqual(44);
      }
    } else {
      expect(dimensions.height).toBeLessThan(44);
    }
  }
});

test("recovers when the local proxy token rotates", async ({ page, baseURL }) => {
  let currentToken = "initial-token";
  let tokenRequests = 0;
  await page.route("**/proxy-token", async (route) => {
    tokenRequests += 1;
    await route.fulfill({ status: 200, contentType: "text/plain", body: currentToken });
  });
  await page.route("**/proxy?url=*", async (route) => {
    if (route.request().headers()["x-oci-zero-proxy"] !== currentToken) {
      await route.fulfill({
        status: 403,
        contentType: "text/plain",
        headers: { "X-OCI-Zero-Proxy-Error": "invalid-token" },
        body: "Invalid proxy token\n",
      });
      return;
    }
    const target = new URL(route.request().url()).searchParams.get("url");
    await route.fulfill({ response: await route.fetch({ url: target }) });
  });

  await page.goto("/");
  await expect(page.locator("#runtime-badge")).toHaveText("Local proxy");
  await page.locator("#repository-input").fill(`${baseURL}/demo/image`);
  await page.getByRole("button", { name: "Browse tags" }).click();
  await expect(page.locator("#status")).toHaveText("Loaded 3 tags for demo/image.");

  currentToken = "rotated-token";
  await page.getByRole("button", { name: "Open version latest" }).click();
  await expect(page.locator("#status")).toHaveText("Ready: 1 verified image layer.");
  expect(tokenRequests).toBe(2);
});

test("selects each platform from an OCI index", async ({ page, baseURL }) => {
  await openImage(page, baseURL, "multi", { index: true });
  await expect(page.locator("#status")).toHaveText("Select one of 2 platform manifests.");
  await expect(page.locator("#export-oci")).toBeHidden();

  await page.getByRole("button", { name: "Select platform linux/amd64" }).click();
  await expect(page.locator("#config-json")).toContainText('"architecture": "amd64"');
  const amdManifest = await page.locator("#manifest-json").textContent();
  await expect(page.locator("#export-oci")).toBeEnabled();
  await expect(page.locator("#export-docker")).toBeEnabled();

  await page.getByRole("button", { name: "Select platform linux/arm64/v8" }).click();
  await expect(page.locator("#config-json")).toContainText('"architecture": "arm64"');
  expect(await page.locator("#manifest-json").textContent()).not.toBe(amdManifest);
  await expect(page.locator("#export-oci")).toBeEnabled();
  await expect(page.locator("#export-docker")).toBeEnabled();
});

test("applies overlay removals and preserves link presentation during downloads", async ({ page, baseURL }) => {
  await openImage(page, baseURL, "overlay");
  await page.locator("#scan-files").click();
  await expect(page.locator("#status")).toHaveText("Scanned 2 layers.");

  await expect(page.locator(".file-row").filter({ hasText: /^remove\.txt/ })).toHaveCount(0);
  await expect(page.locator(".file-row").filter({ hasText: "opaque/old.txt" })).toHaveCount(0);
  await expect(page.locator(".file-row").filter({ hasText: "opaque/new.txt" })).toBeVisible();
  const symlink = page.locator(".file-row").filter({ hasText: "links/symlink.txt" });
  await expect(symlink).toContainText("→ ../shared/target.txt");

  const hardLink = page.locator(".file-row").filter({ hasText: "links/hard.txt" });
  const downloadPromise = page.waitForEvent("download");
  await hardLink.getByRole("button", { name: "Download links/hard.txt" }).click();
  const download = await downloadPromise;
  expect(download.suggestedFilename()).toBe("hard.txt");
  expect(await readFile(await download.path(), "utf8")).toBe("hard-link target contents\n");
  await expect(page.locator("#status")).toContainText("Downloaded links/hard.txt");

  await page.getByRole("button", { name: "Download links/dangling.txt" }).click();
  await expect(page.locator("#status")).toHaveText("Error: Hard-link target missing.txt is not visible");
  await expect(page.locator("#status")).toHaveAttribute("role", "alert");
  await expect(page.locator(".file-row").filter({ hasText: "links/dangling.txt" })).toBeVisible();
});

test("has no serious or critical axe violations initially or when populated", async ({ page, baseURL }) => {
  await expectNoHighImpactViolations(page);
  await openImage(page, baseURL, "latest");
  await page.locator("#scan-files").click();
  await expect(page.locator("#status")).toHaveText("Scanned 1 layer.", { timeout: 10_000 });
  await expectNoHighImpactViolations(page);
});

test("shows a friendly fallback when the registry catalog is unavailable", async ({ page, baseURL }) => {
  await page.route("**/v2/_catalog?**", (route) => route.fulfill({ status: 404, body: "not found\n" }));
  await page.locator("#registry-input").fill(baseURL);
  await page.getByRole("button", { name: "Open catalog" }).click();
  await expect(page.locator("#catalog-results")).toHaveText(/does not expose a catalog.*enter its path directly/);
  await expect(page.locator("#status")).toHaveText(`${new URL(baseURL).host} does not expose /v2/_catalog.`);
  await expect(page.locator("#status")).toHaveAttribute("role", "status");
  await expect(page.getByRole("button", { name: "Load more repositories" })).toBeHidden();
});

test("downloads valid OCI and Docker archive layouts", async ({ page, baseURL }) => {
  await openImage(page, baseURL, "latest");

  const ociDownloadPromise = page.waitForEvent("download");
  await page.getByRole("button", { name: "Download OCI" }).click();
  const ociEntries = parseTar(await readFile(await (await ociDownloadPromise).path()));
  expect([...ociEntries.keys()]).toEqual(expect.arrayContaining(["oci-layout", "index.json"]));
  expect(JSON.parse(ociEntries.get("oci-layout").toString())).toEqual({ imageLayoutVersion: "1.0.0" });
  const ociIndex = JSON.parse(ociEntries.get("index.json").toString());
  expect(ociIndex.manifests).toHaveLength(1);
  expect(ociIndex.manifests[0].annotations["org.opencontainers.image.ref.name"]).toBe("latest");
  const ociManifest = JSON.parse(ociEntries.get(blobPath(ociIndex.manifests[0].digest)).toString());
  expect(ociEntries.has(blobPath(ociManifest.config.digest))).toBe(true);
  expect(ociManifest.layers).toHaveLength(1);
  expect(ociEntries.has(blobPath(ociManifest.layers[0].digest))).toBe(true);

  const dockerDownloadPromise = page.waitForEvent("download");
  await page.getByRole("button", { name: "Download Docker" }).click();
  const dockerEntries = parseTar(await readFile(await (await dockerDownloadPromise).path()));
  expect([...dockerEntries.keys()]).toEqual(expect.arrayContaining([
    "manifest.json", "repositories", "oci-layout", "index.json",
  ]));
  const dockerManifest = JSON.parse(dockerEntries.get("manifest.json").toString());
  expect(dockerManifest[0].RepoTags).toEqual([`${new URL(baseURL).host}/demo/image:latest`]);
  expect(dockerManifest[0].Layers).toHaveLength(1);
  expect(dockerEntries.has(dockerManifest[0].Config)).toBe(true);
  expect(dockerEntries.has(dockerManifest[0].Layers[0])).toBe(true);
  const repositories = JSON.parse(dockerEntries.get("repositories").toString());
  expect(repositories[`${new URL(baseURL).host}/demo/image`].latest).toBeTruthy();
  const dockerIndex = JSON.parse(dockerEntries.get("index.json").toString());
  expect(dockerEntries.has(blobPath(dockerIndex.manifests[0].digest))).toBe(true);
});

test("loads additional catalog and tag pages with deduplication", async ({ page, baseURL }) => {
  await page.locator("#registry-input").fill(baseURL);
  await page.getByRole("button", { name: "Open catalog" }).click();
  await expect(page.locator("#status")).toContainText("Loaded 2 repositories");
  await page.getByRole("button", { name: "Load more repositories" }).click();
  await expect(page.locator("#status")).toContainText("Loaded 3 repositories");
  await page.locator("#catalog-results summary", { hasText: "packages" }).click();
  await page.locator("#catalog-results summary", { hasText: "datadog" }).click();
  await expect(page.getByRole("button", { name: "Open repository packages/datadog/agent" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Load more repositories" })).toBeHidden();

  await page.locator("#repository-input").fill(`${baseURL}/demo/image`);
  await page.getByRole("button", { name: "Browse tags" }).click();
  await expect(page.locator("#tag-limit")).toHaveText("3 matching tags.");
  await page.getByRole("button", { name: "Load more tags" }).click();
  await expect(page.locator("#status")).toHaveText("Loaded 5 tags for demo/image.");
  await expect(page.locator(".version-group")).toHaveCount(5);
  await expect(page.getByRole("button", { name: "Open version overlay" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Load more tags" })).toBeHidden();
  await expect(page.locator("#tag-limit")).toHaveText("5 matching tags.");
});

test("rolls back provisional rows after a declared layer size mismatch", async ({ page, baseURL }) => {
  await openImage(page, baseURL, "size-mismatch");
  await page.locator("#scan-files").click();
  await expect(page.locator(".file-row").filter({ hasText: "etc/hello.txt" })).toBeVisible();
  await expect(page.locator("#status")).toContainText("Error: size mismatch", { ignoreCase: true, timeout: 10_000 });
  await expect(page.locator("#status")).toHaveAttribute("role", "alert");
  await expect(page.locator(".file-row")).toHaveCount(0);
  await expect(page.locator("#layer-results")).toContainText("failed · 0 entries");
});

test("supports repository, version, scan, and download actions from the keyboard", async ({ page, baseURL }) => {
  await page.keyboard.press("Tab");
  await expect(page.locator("#repository-input")).toBeFocused();
  await page.keyboard.press(process.platform === "darwin" ? "Meta+A" : "Control+A");
  await page.keyboard.type(`${baseURL}/demo/image`);
  await page.keyboard.press("Tab");
  await expect(page.getByRole("button", { name: "Browse tags" })).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(page.getByRole("button", { name: "Open version latest" })).toBeVisible();

  await tabTo(page, '[aria-label="Open version latest"]');
  await page.keyboard.press("Space");
  await expect(page.locator("#status")).toHaveText("Ready: 1 verified image layer.");
  await tabTo(page, "#scan-files");
  await page.keyboard.press("Enter");
  await expect(page.locator("#status")).toHaveText("Scanned 1 layer.", { timeout: 10_000 });

  await tabTo(page, '[aria-label="Download etc/hello.txt"]');
  const downloadPromise = page.waitForEvent("download");
  await page.keyboard.press("Space");
  const download = await downloadPromise;
  expect(download.suggestedFilename()).toBe("hello.txt");
  expect(await readFile(await download.path(), "utf8")).toBe("hello from the streamed layer\n");
});

test("live Datadog package extraction", async ({ page }) => {
  test.skip(!process.env.OCI_ZERO_LIVE_E2E, "set OCI_ZERO_LIVE_E2E=1 to run against the local proxy");
  const liveBaseUrl = process.env.OCI_ZERO_LIVE_BASE_URL || "http://127.0.0.1:8000";
  await page.goto(liveBaseUrl);
  await page.locator("#repository-input").fill("install.datadoghq.com/agent-package");
  await page.getByRole("button", { name: "Browse tags" }).click();
  const latestGroup = page.locator(".version-group").filter({ hasText: "latest" });
  const latestChip = latestGroup.getByRole("button", { name: "Open alias latest", exact: true });
  if (await latestChip.count()) await latestChip.click();
  else await latestGroup.getByRole("button", { name: /Open version/ }).click();
  const platform = page.getByRole("button", { name: /Select platform linux\/amd64/ });
  if (await platform.count()) await platform.first().click();
  await page.locator("#scan-files").click();
  await expect(page.locator("#status")).toContainText("Scanned", { timeout: 120_000 });
  const row = page.locator(".file-row").filter({ hasText: "application_monitoring.yaml.example" });
  await expect(row).toBeVisible();
  const downloadPromise = page.waitForEvent("download");
  await row.getByRole("button", { name: /Download .*application_monitoring\.yaml\.example/ }).click();
  const download = await downloadPromise;
  expect(await readFile(await download.path(), "utf8")).toMatch(/Datadog.*APM/i);
});

async function openImage(page, baseURL, tag, options = {}) {
  await page.locator("#repository-input").fill(`${baseURL}/demo/image`);
  await page.getByRole("button", { name: "Browse tags" }).click();
  let tagButton = page.getByRole("button", { name: `Open version ${tag}` });
  if ((await tagButton.count()) === 0) {
    const loadMore = page.getByRole("button", { name: "Load more tags" });
    await expect(loadMore).toBeVisible();
    await loadMore.click();
    tagButton = page.getByRole("button", { name: `Open version ${tag}` });
  }
  await expect(tagButton).toBeVisible();
  await tagButton.click();
  if (!options.index) await expect(page.locator("#status")).toContainText("Ready:");
}

async function expectNoHighImpactViolations(page) {
  const results = await new AxeBuilder({ page }).analyze();
  expect(results.violations.filter(({ impact }) => impact === "serious" || impact === "critical")).toEqual([]);
}

async function tabTo(page, selector) {
  for (let index = 0; index < 100; index += 1) {
    if (await page.locator(selector).evaluate((element) => element === document.activeElement)) return;
    await page.keyboard.press("Tab");
  }
  throw new Error(`Could not reach ${selector} with Tab`);
}

function parseTar(archive) {
  const entries = new Map();
  for (let offset = 0; offset + 512 <= archive.length;) {
    const header = archive.subarray(offset, offset + 512);
    if (header.every((byte) => byte === 0)) break;
    const name = header.subarray(0, 100).toString("utf8").replace(/\0.*$/, "");
    const sizeText = header.subarray(124, 136).toString("ascii").replace(/\0.*$/, "").trim();
    const size = Number.parseInt(sizeText || "0", 8);
    const start = offset + 512;
    entries.set(name, archive.subarray(start, start + size));
    offset = start + Math.ceil(size / 512) * 512;
  }
  return entries;
}

function blobPath(digest) {
  const [algorithm, encoded] = digest.split(":", 2);
  return `blobs/${algorithm}/${encoded}`;
}
