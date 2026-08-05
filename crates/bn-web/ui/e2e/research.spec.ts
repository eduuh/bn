import { expect, test } from "@playwright/test";
import { seed } from "./fixtures";
import { primeToken, shot } from "./helpers";

test.beforeEach(() => seed());

test("research dispatches a scout and shows recent findings", async ({ page }, testInfo) => {
  await primeToken(page);
  await page.goto("/research");

  await expect(page.getByRole("heading", { name: "Research", exact: true })).toBeVisible();

  // An agent-raised ask/answer surfaces below the form.
  await expect(page.getByText(/which crate owns the cron registry/)).toBeVisible();

  await page.locator("#research-q").fill("why is the nvim status_spec flaky?");
  await page.locator("#research-repo").fill("bn");

  await shot(page, testInfo, "research");

  // The primary action dispatches a read-only scout (kind=scout).
  const [req] = await Promise.all([
    page.waitForRequest((r) => r.url().includes("/api/dispatch") && r.method() === "POST"),
    page.getByRole("button", { name: /Start research agent/ }).click(),
  ]);
  expect(JSON.parse(req.postData() ?? "{}")).toMatchObject({ repo: "bn", kind: "scout" });

  // On success it navigates to the Fleet to watch the scout come alive.
  await expect(page.getByRole("heading", { name: "Fleet", exact: true })).toBeVisible();
});
