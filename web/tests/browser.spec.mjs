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
  await expect(page.locator("#runtime-badge")).toBeHidden();
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
  await expect(page.locator("#runtime-badge")).toBeHidden();
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

test("treats a rejected catalog token as an unavailable catalog", async ({ page, baseURL }) => {
  // Docker Hub answers a registry:catalog:* token request with HTTP 400
  // "unknown resource type"; that should degrade to the friendly fallback.
  await page.route("**/v2/_catalog?**", (route) => route.fulfill({
    status: 401,
    headers: { "www-authenticate": 'Bearer realm="https://auth.example.test/token",service="registry.example.test",scope="registry:catalog:*"' },
    body: "unauthorized\n",
  }));
  await page.route("https://auth.example.test/token?**", (route) => route.fulfill({
    status: 400,
    contentType: "application/json",
    body: JSON.stringify({ details: "unknown resource type" }),
  }));

  await page.locator("#registry-input").fill(baseURL);
  await page.getByRole("button", { name: "Open catalog" }).click();
  await expect(page.locator("#catalog-results")).toHaveText(/does not expose a catalog.*enter its path directly/);
  await expect(page.locator("#status")).toHaveText(`${new URL(baseURL).host} does not expose /v2/_catalog.`);
  await expect(page.locator("#status")).not.toHaveClass(/error/);
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
  await expect(page.getByRole("button", { name: "Open repository packages/datadog/agent" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Load more repositories" })).toBeHidden();

  await page.locator("#repository-input").fill(`${baseURL}/demo/image`);
  await page.getByRole("button", { name: "Browse tags" }).click();
  await expect(page.locator("#tag-limit")).toContainText("3 matching groups across 3 loaded tags.");
  await page.getByRole("button", { name: "Load more tags" }).click();
  await expect(page.locator("#status")).toHaveText("Loaded 5 tags for demo/image.");
  await expect(page.locator(".version-group")).toHaveCount(5);
  await expect(page.getByRole("button", { name: "Open version overlay" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Load more tags" })).toBeHidden();
  await expect(page.locator("#tag-limit")).toContainText("5 matching groups across 5 loaded tags.");
});

test("virtualizes large loaded catalogs and tags while limiting manifest concurrency", async ({ page, baseURL }) => {
  const repositories = Array.from({ length: 10_000 }, (_, index) => `namespace/repo-${String(index).padStart(5, "0")}`);
  const tags = Array.from({ length: 10_000 }, (_, index) => `v${String(index).padStart(5, "0")}`);
  let activeManifests = 0;
  let maximumActiveManifests = 0;
  let manifestRequests = 0;
  await page.route("**/v2/_catalog?**", (route) => route.fulfill({
    contentType: "application/json",
    body: JSON.stringify({ repositories }),
  }));
  await page.route("**/v2/huge/tags/list?**", (route) => route.fulfill({
    contentType: "application/json",
    body: JSON.stringify({ name: "huge", tags }),
  }));
  await page.route("**/v2/huge/manifests/**", async (route) => {
    manifestRequests += 1;
    activeManifests += 1;
    maximumActiveManifests = Math.max(maximumActiveManifests, activeManifests);
    await new Promise((resolve) => setTimeout(resolve, 500));
    activeManifests -= 1;
    await route.fulfill({
      contentType: "application/vnd.oci.image.manifest.v1+json",
      body: JSON.stringify({ schemaVersion: 2, config: { digest: `sha256:${"0".repeat(64)}`, size: 0 }, layers: [] }),
    });
  });

  await page.locator("#registry-input").fill(baseURL);
  await page.getByRole("button", { name: "Open catalog" }).click();
  expect(await page.locator("#catalog-results [role=listitem]").count()).toBeLessThan(100);
  await page.locator("#catalog-filter").fill("repo-09999");
  await expect(page.locator("#catalog-count")).toContainText("1 match in 10000 loaded repositories");
  await expect(page.getByRole("button", { name: "Open repository namespace/repo-09999" })).toBeVisible();

  await page.locator("#repository-input").fill(`${baseURL}/huge`);
  await page.getByRole("button", { name: "Browse tags" }).click();
  await expect(page.locator("#tag-limit")).toContainText("10000 loaded tags");
  expect(await page.locator("#tag-results [role=listitem]").count()).toBeLessThan(100);
  await expect(page.getByRole("button", { name: "Open version v09999" })).toBeVisible();
  await page.waitForTimeout(200);
  expect(manifestRequests).toBeLessThan(100);
  expect(maximumActiveManifests).toBeLessThanOrEqual(4);
});

test("deep catalog search follows pages and cancellation is a normal partial result", async ({ page, baseURL }) => {
  await page.locator("#registry-input").fill(baseURL);
  await page.getByRole("button", { name: "Open catalog" }).click();
  await page.locator("#catalog-filter").fill("packages/datadog");
  await expect(page.getByRole("button", { name: "Search remaining pages" })).toBeVisible();
  await page.getByRole("button", { name: "Search remaining pages" }).click();
  await expect(page.getByRole("button", { name: "Open repository packages/datadog/agent" })).toBeVisible();
  await expect(page.locator("#catalog-progress")).toContainText("Search complete");

  let pageNumber = 0;
  await page.route("**/v2/_catalog?**", async (route) => {
    pageNumber += 1;
    if (pageNumber > 1) await new Promise((resolve) => setTimeout(resolve, 1_000));
    await route.fulfill({
      contentType: "application/json",
      headers: { Link: `</v2/_catalog?n=100&last=${pageNumber}>; rel="next"` },
      body: JSON.stringify({ repositories: [`partial/page-${pageNumber}`] }),
    });
  });
  await page.getByRole("button", { name: "Open catalog" }).click();
  await page.locator("#catalog-filter").fill("never-matches");
  await expect(page.getByRole("button", { name: "Search remaining pages" })).toBeVisible();
  await page.getByRole("button", { name: "Search remaining pages" }).click();
  await expect(page.getByRole("button", { name: "Cancel search" })).toBeVisible();
  await page.getByRole("button", { name: "Cancel search" }).click();
  await expect(page.locator("#catalog-progress")).toContainText("Search cancelled");
  await expect(page.locator("#status")).not.toHaveClass(/error/);
});

test("deep tag search loads later names without hiding ordinary tag actions", async ({ page, baseURL }) => {
  await page.locator("#repository-input").fill(`${baseURL}/demo/image`);
  await page.getByRole("button", { name: "Browse tags" }).click();
  await page.locator("#tag-filter").fill("overlay");
  await expect(page.getByRole("button", { name: "Search remaining pages" })).toBeVisible();
  await page.getByRole("button", { name: "Search remaining pages" }).click();
  await expect(page.getByRole("button", { name: "Open version overlay" })).toBeVisible();
  await expect(page.locator("#tags-progress")).toContainText("tag names loaded. Manifests were not fetched by the scan.");
});

test("preserves keyboard focus when progressive digest groups merge", async ({ page, baseURL }) => {
  await page.route("**/v2/packages/datadog/agent/manifests/**", async (route) => {
    await new Promise((resolve) => setTimeout(resolve, 250));
    await route.continue();
  });
  await page.locator("#repository-input").fill(`${baseURL}/packages/datadog/agent`);
  await page.getByRole("button", { name: "Browse tags" }).click();
  await page.getByRole("button", { name: "Open version 7", exact: true }).focus();
  await expect(page.locator(".version-group")).toHaveCount(1, { timeout: 5_000 });
  expect(await page.evaluate(() => document.activeElement?.closest(".version-group")?.textContent)).toContain("7.81.1-1");
});

test("applies list sorting and system dark colors", async ({ page, baseURL }) => {
  await page.locator("#registry-input").fill(baseURL);
  await page.getByRole("button", { name: "Open catalog" }).click();
  await page.locator("#catalog-sort").selectOption("name-desc");
  await expect(page.locator("#catalog-results [role=listitem]").first()).toContainText("demo/nested/one");

  await page.locator("#repository-input").fill(`${baseURL}/demo/image`);
  await page.getByRole("button", { name: "Browse tags" }).click();
  await page.locator("#tag-sort").selectOption("name-asc");
  await expect(page.locator("#tag-results [role=listitem]").first().locator("strong")).toHaveText("corrupt");

  await page.emulateMedia({ colorScheme: "dark" });
  expect(await page.evaluate(() => getComputedStyle(document.documentElement).backgroundColor)).toBe("rgb(21, 24, 29)");
});

test("paginates every matching file in 250-row pages", async ({ page, baseURL }) => {
  await page.locator("#repository-input").fill(`${baseURL}/demo/many`);
  await page.getByRole("button", { name: "Browse tags" }).click();
  await page.getByRole("button", { name: "Open version latest" }).click();
  await expect(page.locator("#status")).toContainText("Ready:");
  await page.locator("#scan-files").click();
  await expect(page.locator("#status")).toHaveText("Scanned 1 layer.", { timeout: 10_000 });
  await expect(page.locator(".file-row")).toHaveCount(250);
  await expect(page.locator("#file-limit")).toHaveText("Showing 1–250 of 600 matching entries.");
  await page.getByRole("button", { name: "Next" }).click();
  await expect(page.locator("#file-limit")).toHaveText("Showing 251–500 of 600 matching entries.");
  await page.getByRole("button", { name: "Next" }).click();
  await expect(page.locator(".file-row")).toHaveCount(100);
  await expect(page.locator("#file-limit")).toHaveText("Showing 501–600 of 600 matching entries.");
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
  test.skip(!process.env.OCI_ZERO_LIVE_E2E, "set OCI_ZERO_LIVE_E2E=1 to run against a live registry in a CORS-disabled browser");
  const liveBaseUrl = process.env.OCI_ZERO_LIVE_BASE_URL || "http://127.0.0.1:8080";
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
