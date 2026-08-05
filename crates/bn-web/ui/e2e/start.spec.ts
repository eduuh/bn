import { expect, test } from "@playwright/test";
import { seed } from "./fixtures";
import { primeToken, shot } from "./helpers";

test.beforeEach(() => seed());

test("start an agent — the first-class dispatch flow", async ({ page }, testInfo) => {
  await primeToken(page);
  await page.goto("/start");

  await expect(page.getByRole("heading", { name: "Start an agent" })).toBeVisible();

  await page.locator("#start-item").fill("542");
  await page.locator("#start-repo").fill("bn");

  await shot(page, testInfo, "start");

  // Clicking the primary button issues a real POST /api/dispatch with the chosen fields.
  const [req] = await Promise.all([
    page.waitForRequest((r) => r.url().includes("/api/dispatch") && r.method() === "POST"),
    page.getByRole("button", { name: /Start agent/ }).click(),
  ]);
  expect(JSON.parse(req.postData() ?? "{}")).toMatchObject({
    repo: "bn",
    item: "542",
    harness: "copilot",
    kind: "ship",
  });

  // On success the UI navigates to the Fleet so you watch it come alive.
  await expect(page.getByRole("heading", { name: "Fleet", exact: true })).toBeVisible();
});

test("scout mode dispatches with kind=scout", async ({ page }) => {
  await primeToken(page);
  await page.goto("/start");
  await page.locator("#start-item").fill("investigate the flaky nvim spec");
  await page.locator("#start-repo").fill("bn");
  await page.getByRole("button", { name: "scout" }).click();
  const [req] = await Promise.all([
    page.waitForRequest((r) => r.url().includes("/api/dispatch") && r.method() === "POST"),
    page.getByRole("button", { name: /Start agent/ }).click(),
  ]);
  expect(JSON.parse(req.postData() ?? "{}").kind).toBe("scout");
});
