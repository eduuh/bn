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

  // One primary nav in both layouts now — the rail re-flows into the phone tab bar rather than a
  // second, separate nav existing for small screens (clay redesign, #966).
  const nav = page.getByRole("navigation", { name: "primary" });
  await expect(nav).toBeVisible();
  await expect(nav.getByRole("link", { name: /Start/ }).first()).toBeVisible();

  await shot(page, testInfo, "fleet");
});
