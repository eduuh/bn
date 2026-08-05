import { expect, test } from "@playwright/test";
import { seed } from "./fixtures";
import { primeToken, shot } from "./helpers";

test.beforeEach(() => seed());

test("the command center is the index: unified actions + one-click Start agent", async ({
  page,
}, testInfo) => {
  await primeToken(page);
  await page.goto("/"); // `/` is now the action-centric command center (#522)

  // Header + the action-first framing.
  await expect(page.getByRole("heading", { name: "Command center", exact: true })).toBeVisible();

  // The glance strip pins the actionable + urgent counts.
  await expect(page.getByText("actionable", { exact: true })).toBeVisible();
  await expect(page.getByText("urgent", { exact: true }).first()).toBeVisible();

  // Seeded signals surface as rows: an urgent attention item and the failing cron.
  await expect(page.getByText("Prod dashboard is 500ing")).toBeVisible();
  await expect(page.getByText("Failing cron · refresh-all")).toBeVisible();

  // Every row carries a one-click Start-agent CTA.
  const starts = page.getByRole("button", { name: /Start agent/ });
  expect(await starts.count()).toBeGreaterThan(0);

  await shot(page, testInfo, "command-center");
});

test("Start agent opens a composer pre-filled with an editable suggested prompt", async ({
  page,
}) => {
  await primeToken(page);
  await page.goto("/");

  // Open the composer from the first actionable row.
  await page.getByRole("button", { name: /Start agent/ }).first().click();

  // The modal is up, and the prompt textarea is pre-filled (a starting point from the owner).
  const dialog = page.getByRole("dialog");
  await expect(dialog).toBeVisible();
  const prompt = dialog.locator("#composer-prompt");
  await expect(prompt).toBeVisible();
  expect((await prompt.inputValue()).trim().length).toBeGreaterThan(0);

  // The urgency toggle offers dispatch-now and queue.
  await expect(dialog.getByRole("button", { name: /Urgent/ })).toBeVisible();
  await expect(dialog.getByRole("button", { name: /Queue/ })).toBeVisible();

  // Escape closes it.
  await page.keyboard.press("Escape");
  await expect(dialog).toBeHidden();
});

test("the mobile drawer opens the grouped sidebar nav", async ({ page }, testInfo) => {
  test.skip(testInfo.project.name !== "mobile", "drawer is the mobile-only nav");
  await primeToken(page);
  await page.goto("/");

  // The sidebar is off-canvas until the hamburger opens the drawer.
  await page.getByRole("button", { name: "open navigation" }).click();
  await expect(page.getByRole("complementary", { name: "primary navigation" })).toBeVisible();
  await expect(page.getByText("items need you")).toBeVisible();
  // Grouped surfaces are present.
  await expect(page.getByRole("link", { name: "Cost" })).toBeVisible();
  await expect(page.getByRole("link", { name: "Cron" })).toBeVisible();
});
