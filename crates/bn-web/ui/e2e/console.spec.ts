import { expect, test } from "@playwright/test";
import { seed } from "./fixtures";
import { primeToken, shot } from "./helpers";

test.beforeEach(() => seed());

test("the console is the index: one queue, grouped by what you'd do about it", async ({
  page,
}, testInfo) => {
  await primeToken(page);
  await page.goto("/");

  // The left column heads as "Queue" — "Console" is the rail entry that got you here, and having
  // the screen repeat its own nav label spent the one display-serif line on no information.
  const queue = page.getByRole("complementary", { name: "work queue" });
  await expect(queue.getByRole("heading", { name: "Queue", exact: true })).toBeVisible();

  // The three lanes are always present — an empty lane reassures rather than disappearing.
  await expect(queue.getByRole("heading", { name: "Blocked on you" })).toBeVisible();
  await expect(queue.getByRole("heading", { name: "Running" })).toBeVisible();
  await expect(queue.getByRole("heading", { name: "Backlog" })).toBeVisible();

  // Starting work is always one click away, on every selection.
  await expect(queue.getByRole("button", { name: /Start/ })).toBeVisible();

  await shot(page, testInfo, "console");
});

test("selecting a row fills the focus pane without navigating away from the queue", async ({
  page,
}, testInfo) => {
  test.skip(testInfo.project.name !== "desktop", "the queue only stays on screen at lg and up");
  await primeToken(page);
  await page.goto("/");

  const queue = page.getByRole("complementary", { name: "work queue" });
  // The queue auto-selects the most-blocked row, so something is always in focus.
  await expect(page).toHaveURL(/\?sel=/);

  const rows = queue.locator("button[aria-current]");
  expect(await rows.count()).toBeGreaterThan(0);

  // Picking a different row swaps the focus pane but keeps the queue on screen.
  await rows.nth(1).click();
  await expect(queue).toBeVisible();
  await expect(page).toHaveURL(/\?sel=/);
});

test("on a phone the console is list-or-detail, with a way back", async ({ page }, testInfo) => {
  test.skip(testInfo.project.name !== "mobile", "the swap only happens below lg");
  await primeToken(page);
  await page.goto("/");

  const queue = page.getByRole("complementary", { name: "work queue" });
  // Nothing is auto-selected on a phone — you land on the list.
  await expect(queue).toBeVisible();
  await expect(page).not.toHaveURL(/\?sel=/);

  const rows = queue.locator("button[aria-current]");
  test.skip((await rows.count()) === 0, "queue is empty at this point in the run");

  // Selecting replaces the queue with the detail rather than stacking both.
  await rows.first().click();
  await expect(queue).toBeHidden();

  // And the back control returns to the list.
  await page.getByRole("button", { name: "Queue" }).click();
  await expect(queue).toBeVisible();
  await expect(page).not.toHaveURL(/\?sel=/);
});

test("j and k walk the queue from the keyboard", async ({ page }, testInfo) => {
  test.skip(testInfo.project.name !== "desktop", "keyboard walking is a desktop affordance");
  await primeToken(page);
  await page.goto("/");
  await expect(page).toHaveURL(/\?sel=/);

  // The suite shares one mutable server, so an earlier spec may have killed agents or acked items;
  // walking needs at least two rows to be meaningful.
  const queue = page.getByRole("complementary", { name: "work queue" });
  const rows = await queue.locator("button[aria-current]").count();
  test.skip(rows < 2, "needs at least two queue rows to walk");

  const first = new URL(page.url()).searchParams.get("sel");
  await page.keyboard.press("j");
  await expect(page).not.toHaveURL(new RegExp(`sel=${encodeURIComponent(first ?? "")}$`));
  const second = new URL(page.url()).searchParams.get("sel");
  expect(second).not.toBe(first);

  await page.keyboard.press("k");
  await expect
    .poll(() => new URL(page.url()).searchParams.get("sel"))
    .toBe(first);
});

test("a plan review offers a decision, not another agent", async ({ page }) => {
  await primeToken(page);
  await page.goto("/");

  const queue = page.getByRole("complementary", { name: "work queue" });
  const review = queue.getByRole("button").filter({ hasText: /Plan review/i }).first();
  test.skip((await review.count()) === 0, "no seeded plan review in this fixture");

  await review.click();
  // Approve / request-changes / reject — the decision the item actually admits.
  await expect(page.getByRole("button", { name: /Approve/ })).toBeVisible();
});

test("the filter narrows the queue in place", async ({ page }) => {
  await primeToken(page);
  await page.goto("/");

  const queue = page.getByRole("complementary", { name: "work queue" });
  const before = await queue.locator("button[aria-current]").count();
  test.skip(before === 0, "nothing left in the queue to filter at this point in the run");

  await queue.getByLabel("filter the queue").fill("zzz-no-such-item");
  const after = await queue.locator("button[aria-current]").count();
  expect(after).toBeLessThan(before);
  await expect(queue.getByText(/No matches in this lane/).first()).toBeVisible();
});

test("at tablet width Act folds under Focus — one pane, not a hidden column plus a duplicate", async ({
  page,
}, testInfo) => {
  test.skip(testInfo.project.name !== "desktop", "the fold is a window-width behaviour");
  await primeToken(page);
  // Between `lg` and `wide`: the third column no longer fits, so Act becomes a horizontal strip.
  await page.setViewportSize({ width: 1100, height: 900 });
  await page.goto("/");

  // Exactly one — the console used to render a second ActPane below the fold, which meant a
  // half-typed steer message existed twice and was lost on crossing the breakpoint.
  const act = page.getByRole("complementary", { name: "actions" });
  await expect(act).toHaveCount(1);
  await expect(act).toBeVisible();

  // It sits under the focus pane, flush with its left edge — not beside it.
  const focus = await page.getByRole("region", { name: "focus" }).boundingBox();
  const box = await act.boundingBox();
  expect(box!.y).toBeGreaterThan(focus!.y);
  expect(Math.abs(box!.x - focus!.x)).toBeLessThan(4);
});
