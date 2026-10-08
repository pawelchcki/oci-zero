import { readFile } from "node:fs/promises";
import { expect, test } from "@playwright/test";

const registry = process.env.OCI_ZERO_DEMO_REGISTRY_URL;
test.skip(!registry, "Set OCI_ZERO_DEMO_REGISTRY_URL to test a live Rust/Worker demo");

test("in-memory registry: browser scans overlays and downloads repository files across origins", async ({ page }) => {
  await page.goto("/");
  // Wait for Wasm initialization and event handlers before submitting a form.
  await expect(page.getByRole("button", { name: "Docker Hub", exact: true })).toBeAttached();
  await page.locator("#registry-input").fill(registry);
  await page.getByRole("button", { name: "Open catalog", exact: true }).click();
  await page.getByRole("button", { name: "Open repository demo/garden", exact: true }).click();
  const group = page.locator(".version-group").filter({ hasText: "latest" });
  await expect(group).toContainText("v2");
  await group.getByRole("button", { name: "Open version latest", exact: true }).click();
  await page.locator("#scan-files").click();
  await expect(page.locator("#status")).toHaveText("Scanned 2 layers.");
  const table = page.getByRole("table", { name: "Merged filesystem entries" });
  await expect(table.getByRole("rowheader", { name: "garden/flower.txt", exact: true })).toBeVisible();
  await expect(table.getByRole("rowheader", { name: "garden/seed.txt", exact: true })).toHaveCount(0);
  const downloadPromise = page.waitForEvent("download");
  await table.getByRole("button", { name: "Download hello.txt", exact: true }).click();
  const download = await downloadPromise;
  expect(await readFile(await download.path(), "utf8")).toBe("Hello from the second layer!\n");

  await page.locator("#repository-input").fill(`${registry}/demo/source`);
  await page.getByRole("button", { name: "Browse tags", exact: true }).click();
  await page.getByRole("button", { name: "Open version latest", exact: true }).click();
  await page.locator("#scan-files").click();
  await expect(page.locator("#status")).toHaveText("Scanned 1 layer.");
  await expect(table.getByRole("rowheader", { name: "README.md", exact: true })).toBeVisible();
  await expect(table.getByRole("rowheader", { name: "memfs/note.txt", exact: true })).toBeVisible();
  const readmePromise = page.waitForEvent("download");
  await table.getByRole("button", { name: "Download README.md", exact: true }).click();
  const readme = await readmePromise;
  expect(await readFile(await readme.path(), "utf8")).toBe(await readFile(new URL("../../README.md", import.meta.url), "utf8"));
});
