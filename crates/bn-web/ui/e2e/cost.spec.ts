import { expect, test } from "@playwright/test";
import { seed } from "./fixtures";
import { primeToken, shot } from "./helpers";

test.beforeEach(() => seed());

test("cost view renders totals + a by-category breakdown", async ({ page }, testInfo) => {
  await primeToken(page);
  await page.goto("/cost");

  await expect(page.getByRole("heading", { name: /Cost/ })).toBeVisible();
  await expect(page.getByText("AIC · Copilot bill")).toBeVisible();
  await expect(page.getByText("$ · API-equiv")).toBeVisible();

  // The seeded agent_session rows fold into work-class categories.
  await expect(page.getByText("Coding").first()).toBeVisible();
  await expect(page.getByText("Orchestration").first()).toBeVisible();

  await shot(page, testInfo, "cost");
});
