import { expect, test } from "@playwright/test";
import { seed } from "./fixtures";
import { primeToken, shot } from "./helpers";

test.beforeEach(() => seed());

test("repos view lists managed repos with rollups and drills into a repo's sessions", async ({ page }, testInfo) => {
  await primeToken(page);
  await page.goto("/repos");

  await expect(page.getByRole("heading", { name: "Repos", exact: true })).toBeVisible();

  // Every repo bn knows: `bn` (seeded notes + orchestrate agents) and the managed-but-empty `forge`.
  const bnCard = page.locator('a[href="/repos/bn"]');
  const forgeCard = page.locator('a[href="/repos/forge"]');
  await expect(bnCard).toBeVisible();
  await expect(forgeCard).toBeVisible();

  // bn's rollup: the controller plus 3 in-flight dispatches, and 2 recent note-only sessions.
  await expect(bnCard).toContainText("4 active");
  await expect(bnCard).toContainText("6 sessions");
  await expect(bnCard).toContainText("2 PRs");
  // forge is managed with no sessions yet.
  await expect(forgeCard).toContainText("managed");
  await expect(forgeCard).toContainText("0 sessions");

  await shot(page, testInfo, "repos");

  // Drill in → the repo's live agents and recent note-only branches.
  await bnCard.click();
  await expect(page).toHaveURL(/\/repos\/bn$/);
  await expect(page.getByRole("heading", { name: "bn", exact: true })).toBeVisible();

  await expect(
    page.getByRole("heading", { name: "Agents targeting this repo", exact: true }),
  ).toBeVisible();
  await expect(page.getByRole("heading", { name: "Recent branches", exact: true })).toBeVisible();

  // Active: the controller plus dispatched branches from the orchestrate run.
  await expect(page.getByText("dispatch-web-redesign-500", { exact: true })).toBeVisible();
  await expect(page.getByText("dispatch-dns-505", { exact: true })).toBeVisible();

  // Recent: the two seeded branch notes, each with its PR + todo/blocker badges.
  await expect(page.getByText("feature-cost-panel", { exact: true })).toBeVisible();
  await expect(page.getByText("docs-readme", { exact: true })).toBeVisible();
  await expect(page.getByText("#460")).toBeVisible();
  await expect(page.getByText("#455")).toBeVisible();
  await expect(page.getByText("2 todo")).toBeVisible();
  await expect(page.getByText("1 blocked")).toBeVisible();

  await shot(page, testInfo, "repo-detail");
});
