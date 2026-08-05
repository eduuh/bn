import { expect, test } from "@playwright/test";
import { seed } from "./fixtures";
import { primeToken, shot } from "./helpers";

test.beforeEach(() => seed());

test("today is the default landing: summary, aimed goals + quarter alignment", async ({
  page,
}, testInfo) => {
  await primeToken(page);
  await page.goto("/today"); // Today re-homed under the Today group; `/` is now the command center (#522)

  // Header + the aimed-vs-achieved framing (scoped to the page header — the phrase also reads
  // naturally inside goal text, so target the subtitle to avoid a strict-mode double match).
  await expect(page.getByRole("heading", { name: "Today", exact: true })).toBeVisible();
  await expect(page.getByText(/^\d{4}-\d{2}-\d{2} · aimed vs achieved$/)).toBeVisible();

  // Top-of-day summary tiles.
  await expect(page.getByText("goals done", { exact: true }).first()).toBeVisible();
  await expect(page.getByText("PRs merged", { exact: true }).first()).toBeVisible();
  await expect(page.getByText("agents today", { exact: true }).first()).toBeVisible();
  await expect(page.getByText("AIC spent", { exact: true }).first()).toBeVisible();

  // Aimed today: the seeded open daily goal, with the quarter-alignment chip on the aligned goal.
  await expect(page.getByRole("heading", { name: "Aimed today" })).toBeVisible();
  await expect(page.getByText("Ship the surface-all dashboard")).toBeVisible();
  await expect(page.getByText("◇ surface-all")).toBeVisible();

  // Quarter goals render beneath the daily list.
  await expect(
    page.getByText("Make the dashboard the single pane for everything bn"),
  ).toBeVisible();

  // The achieved column exists (its PRs/commits come from git/gh live, so not asserted here).
  await expect(page.getByRole("heading", { name: "Achieved today" })).toBeVisible();

  await shot(page, testInfo, "today");
});

test("fleet moved off the index to /fleet", async ({ page }) => {
  await primeToken(page);
  await page.goto("/fleet");
  await expect(page.getByRole("heading", { name: "Fleet", exact: true })).toBeVisible();
});
