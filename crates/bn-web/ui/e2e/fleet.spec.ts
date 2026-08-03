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

  // Two surfaces, one board: a card is spent only where a decision is waiting, so the working tier
  // is rows. A needs-you card carries its repairs; a working row carries none.
  const needs = page.getByRole("region", { name: "Needs you" });
  const working = page.getByRole("region", { name: "Working" });
  await expect(needs.getByRole("button", { name: /Relaunch/ }).first()).toBeVisible();
  await expect(working.getByRole("button", { name: /Relaunch/ })).toHaveCount(0);

  // The harness is a chip that discloses a menu — never a permanent segmented block on every card.
  await expect(page.getByRole("menu", { name: "Switch harness" })).toHaveCount(0);
  const harnessChip = needs.getByRole("button", { name: /claude|copilot/ }).first();
  await harnessChip.click();
  await expect(page.getByRole("menu", { name: "Switch harness" })).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("menu", { name: "Switch harness" })).toHaveCount(0);

  // One primary nav in both layouts now — the rail re-flows into the phone tab bar rather than a
  // second, separate nav existing for small screens (clay redesign, #966).
  const nav = page.getByRole("navigation", { name: "primary" });
  await expect(nav).toBeVisible();
  // The pinned verb is "New task", not "Start an agent": work only ever starts from a task (#1006),
  // so the rail stopped carrying a Start link. Starting an agent is the page's own header button.
  await expect(nav.getByRole("link", { name: /New task/ }).first()).toBeVisible();
  await expect(nav.getByRole("link", { name: "Fleet" }).first()).toBeVisible();

  await shot(page, testInfo, "fleet");
});
