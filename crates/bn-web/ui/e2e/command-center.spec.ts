import { expect, test } from "@playwright/test";
import { seed } from "./fixtures";
import { primeToken, shot } from "./helpers";

test.beforeEach(() => seed());

test("the command center is the index: unified actions + one-click Start agent", async ({
  page,
}, testInfo) => {
  await primeToken(page);
  await page.goto("/inbox"); // the classic action list now lives at /inbox; `/` is the console

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
  await page.goto("/inbox");

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

test("the phone tab-bar carries every destination, with no drawer to open", async ({
  page,
}, testInfo) => {
  test.skip(testInfo.project.name !== "mobile", "the tab-bar shape is the phone layout");
  await primeToken(page);
  await page.goto("/inbox");

  // The clay redesign removed the hamburger drawer outright: the rail *is* the bottom tab bar on a
  // phone, so every destination is reachable without opening anything first.
  await expect(page.getByRole("button", { name: "open navigation" })).toHaveCount(0);

  const nav = page.getByRole("navigation", { name: "primary" });
  await expect(nav).toBeVisible();
  await expect(nav.getByRole("link", { name: /Console/ })).toBeVisible();
  // Destinations from every group, including the ones that used to sit behind the drawer. The
  // Insight group's entry is Insights — cost is a tab of that page, not its own destination.
  await expect(nav.getByRole("link", { name: "Insights" })).toBeVisible();
  await expect(nav.getByRole("link", { name: "Cron" })).toBeVisible();
  await expect(nav.getByRole("link", { name: "Handoff" })).toBeVisible();
});
