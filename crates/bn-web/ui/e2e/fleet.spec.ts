import { expect, test } from "@playwright/test";
import { seed } from "./fixtures";
import { primeToken, shot } from "./helpers";

test.beforeEach(() => seed());

test("fleet renders live agents, needs-you, and the bottom tab-bar", async ({ page }, testInfo) => {
  await primeToken(page);
  await page.goto("/fleet");

  await expect(page.getByRole("heading", { name: "Fleet", exact: true })).toBeVisible();

  // "Orchestrators" pins the live wave-runner (#500) in a distinct top tier, with its role badge.
  await expect(page.getByRole("heading", { name: "Orchestrators", exact: true })).toBeVisible();
  const orchestrators = page.getByRole("region", { name: "Orchestrators" });
  await expect(orchestrators.getByText("◆ Orchestrator", { exact: true })).toBeVisible();

  // "Needs you" pins the plan-waiting (501) and errored (505) agents at the top.
  await expect(page.getByText("Needs you")).toBeVisible();
  await expect(page.getByText("dispatch/plan-review-501")).toBeVisible();
  await expect(page.getByText("dispatch/dns-505")).toBeVisible();

  // A plain working agent shows in the Working section.
  await expect(page.getByText("dispatch/web-redesign-500")).toBeVisible();

  // Primary nav: bottom tab-bar on phones, the grouped sidebar on desktop (#522). "Start" lives in
  // both navs by design, so scope the assertion to the nav that's active for this viewport.
  if (testInfo.project.name === "mobile") {
    const nav = page.getByRole("navigation", { name: "primary" });
    await expect(nav).toBeVisible();
    await expect(nav.getByRole("link", { name: "Start" })).toBeVisible();
  } else {
    const nav = page.getByRole("complementary", { name: "primary navigation" });
    await expect(nav).toBeVisible();
    await expect(nav.getByRole("link", { name: "Start", exact: true })).toBeVisible();
  }

  await shot(page, testInfo, "fleet");
});
