import { expect, test } from "@playwright/test";
import { seed } from "./fixtures";
import { shot } from "./helpers";

test.beforeEach(() => seed());

test("goals show today's aimed-vs-achieved and the quarter status", async ({ page }, testInfo) => {
  await page.goto("/goals");

  await expect(page.getByRole("heading", { name: "Goals", exact: true })).toBeVisible();

  // Daily goal with its aimed-vs-achieved durations.
  const g1 = page.getByRole("listitem").filter({ hasText: "Ship the surface-all dashboard" });
  await expect(g1).toBeVisible();
  await expect(g1.getByText("50m / 90m")).toBeVisible();

  // Quarter section: a done goal shows a "done" status badge.
  const done = page
    .getByRole("listitem")
    .filter({ hasText: "Ship the mobile-first web redesign" });
  await expect(done).toBeVisible();
  await expect(done.getByText("done", { exact: true })).toBeVisible();

  await shot(page, testInfo, "goals");
});

test("a daily goal renders its connected note and jumps to it (#564)", async ({ page }) => {
  await page.goto("/goals");

  // g1 carries a connected branch note that links into the Notes view.
  const g1 = page.getByRole("listitem").filter({ hasText: "Ship the surface-all dashboard" });
  const jump = g1.getByRole("link", { name: /bn\/feature-cost-panel/ });
  await expect(jump).toBeVisible();
  await expect(jump).toHaveAttribute("href", "/notes?repo=bn&branch=feature-cost-panel");

  // Following it lands on the Notes view with that branch note preselected (its goal renders).
  await jump.click();
  await expect(page).toHaveURL(/\/notes\?repo=bn&branch=feature-cost-panel/);
  await expect(page.getByText("Surface per-repo cost on the dashboard.")).toBeVisible();
});

test("the connect-note dialog offers link + attach modes (#564)", async ({ page }) => {
  await page.goto("/goals");

  const g2 = page.getByRole("listitem").filter({ hasText: "Review the plan-review PRs" });
  await g2.getByRole("button", { name: "Connect note" }).click();

  const dialog = page.getByRole("dialog");
  await expect(dialog.getByText("Connect a note")).toBeVisible();
  await expect(dialog.getByRole("button", { name: /Link a branch note/ })).toBeVisible();
  await expect(dialog.getByRole("button", { name: /Attach text/ })).toBeVisible();
});
