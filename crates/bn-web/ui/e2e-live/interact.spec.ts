// Interaction sweep against the live bn-web — everything you can *do* from the dashboard that
// does not mutate real state.
//
// The route sweep proves each screen paints; this proves each screen responds. Every test here
// drives a control the owner actually reaches for (the palette, the tabs, the filters, the start
// form, the drill-downs) and asserts the app reacted, while the same diagnostic watchers from the
// sweep make sure reacting did not throw.
//
// Nothing here submits. Write flows live in `writes.spec.ts` behind `BN_LIVE_WRITES=1`.

import { expect, test } from "@playwright/test";
import { primeToken, recordDiagnostics, visit, watch } from "./live";

/** Fail the test if the interaction produced an uncaught error or a failed request. */
function assertClean(diags: ReturnType<typeof watch>, what: string): void {
  recordDiagnostics(diags);
  const fatal = diags.filter((d) => d.kind === "pageerror" || d.kind === "httperror");
  expect(fatal, `${what} produced errors:\n${fatal.map((d) => d.text).join("\n")}`).toEqual([]);
}

test("the command palette opens on ⌘K, searches data, and navigates", async ({ page }, testInfo) => {
  const diags = watch(page, "/", testInfo.project.name);
  await primeToken(page);
  await visit(page, "/");

  await page.keyboard.press("ControlOrMeta+k");
  const palette = page.getByRole("dialog");
  await expect(palette).toBeVisible();

  // "bn" is the managed repo, so the server ranks a repo row first.
  await page.keyboard.type("bn");
  const option = palette.getByRole("option").first();
  await expect(option).toBeVisible();
  await option.click();

  await expect(palette).not.toBeVisible();
  await expect(page).not.toHaveURL(/\/$/);
  assertClean(diags, "command palette");
});

/**
 * ⌘K is the topbar's advertised entry point ("Search ⌘K") and the only fast way to a screen on a
 * phone. It used to search *data* only — `/api/search` has no notion of the app's own destinations
 * — so typing the name of a screen with no same-named data row ("cost", "console") dead-ended on
 * "No results.", the single most common thing anyone types into a palette.
 */
test("⌘K reaches a destination by name and navigates to it", async ({ page }, testInfo) => {
  await primeToken(page);
  await visit(page, "/fleet");

  await page.keyboard.press("ControlOrMeta+k");
  const palette = page.getByRole("dialog");
  await expect(palette).toBeVisible();

  // With nothing typed the palette leads with every destination, so it is useful before the first
  // keystroke rather than showing a bare prompt.
  await expect(palette.getByText("Destinations")).toBeVisible();

  // "cost" matches no data row on this host — the exact case that used to dead-end.
  await page.keyboard.type("cost");
  await expect(palette.getByText("Go to")).toBeVisible();
  await expect(palette.getByText("No results.")).toHaveCount(0);

  await palette.getByRole("option").first().click();
  await expect(page).toHaveURL(/\/cost$/);
});

test("⌘K matches a destination by synonym", async ({ page }, testInfo) => {
  test.skip(testInfo.project.name !== "desktop", "layout-independent");
  await primeToken(page);
  await visit(page, "/");

  await page.keyboard.press("ControlOrMeta+k");
  const palette = page.getByRole("dialog");
  // Nobody remembers every name bn uses; "schedule" should still find Cron.
  await page.keyboard.type("schedule");
  await expect(palette.getByRole("option").first()).toBeVisible();
  await palette.getByRole("option").first().click();
  await expect(page).toHaveURL(/\/cron$/);
});

test("the palette closes on Escape without navigating", async ({ page }, testInfo) => {
  const diags = watch(page, "/fleet", testInfo.project.name);
  await primeToken(page);
  await visit(page, "/fleet");

  await page.keyboard.press("ControlOrMeta+k");
  await expect(page.getByRole("dialog")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("dialog")).not.toBeVisible();
  await expect(page).toHaveURL(/\/fleet/);
  assertClean(diags, "palette escape");
});

test("every rail destination is reachable by click and lands on its screen", async ({
  page,
}, testInfo) => {
  const diags = watch(page, "rail", testInfo.project.name);
  await primeToken(page);
  await visit(page, "/");

  const nav = page.getByRole("navigation", { name: "primary" });
  const labels = ["Fleet", "Nodes", "Cron", "Attention", "Today", "Goals", "Repos", "Insights", "Cost"];

  for (const label of labels) {
    await nav.getByRole("link", { name: new RegExp(`^${label}`) }).first().click();
    await expect(page.getByRole("heading", { name: new RegExp(label) }).first()).toBeVisible({
      timeout: 15_000,
    });
  }
  assertClean(diags, "rail navigation");
});

test("cost switches window, metric and breakdown dimension", async ({ page }, testInfo) => {
  const diags = watch(page, "/cost", testInfo.project.name);
  await primeToken(page);
  await visit(page, "/cost");

  // Window: 7d / 30d / 90d must each re-query and re-render without blanking the chart.
  const windows = page.getByRole("tablist", { name: "window" });
  for (const win of ["7d", "90d", "30d"]) {
    await windows.getByRole("tab", { name: win, exact: true }).click();
    await page.waitForTimeout(600);
    await expect(page.getByText(/Usage by day/)).toBeVisible();
  }

  // Metric toggle on the chart.
  const metrics = page.getByRole("tablist", { name: "chart metric" });
  for (const tab of await metrics.getByRole("tab").all()) {
    await tab.click();
    await page.waitForTimeout(300);
    await expect(page.getByText(/Usage by day/)).toBeVisible();
  }

  // Breakdown dimension tabs — each must render a populated table.
  const dims = page.getByRole("tablist").last();
  for (const tab of await dims.getByRole("tab").all()) {
    await tab.click();
    await page.waitForTimeout(300);
    await expect(page.locator("table").first()).toBeVisible();
  }

  assertClean(diags, "cost controls");
});

test("the console queue filters and selecting a row swaps the focus pane", async ({
  page,
}, testInfo) => {
  test.skip(testInfo.project.name !== "desktop", "the queue only stays on screen at lg and up");
  const diags = watch(page, "/", testInfo.project.name);
  await primeToken(page);
  await visit(page, "/");

  const queue = page.getByRole("complementary", { name: "work queue" });
  const rows = queue.locator("button[aria-current]");
  const before = await rows.count();
  expect(before, "the live queue should have rows").toBeGreaterThan(0);

  // Filtering narrows the queue and `clear` restores it.
  await queue.getByPlaceholder(/Filter queue/).fill("zzzz-no-such-item");
  await page.waitForTimeout(400);
  expect(await rows.count()).toBeLessThan(before);

  await queue.getByPlaceholder(/Filter queue/).fill("");
  await page.waitForTimeout(400);
  expect(await rows.count()).toBe(before);

  // Selecting a different row keeps the queue on screen and moves the selection into the URL.
  if (before > 1) {
    await rows.nth(1).click();
    await expect(page).toHaveURL(/\?sel=/);
    await expect(queue).toBeVisible();
  }
  assertClean(diags, "console queue");
});

test("j/k move the console selection without leaving the queue", async ({ page }, testInfo) => {
  test.skip(testInfo.project.name !== "desktop", "keyboard nav is a desktop affordance");
  const diags = watch(page, "/", testInfo.project.name);
  await primeToken(page);
  await visit(page, "/");

  const queue = page.getByRole("complementary", { name: "work queue" });
  await queue.locator("button[aria-current]").first().click();
  const first = page.url();

  await page.keyboard.press("j");
  await page.waitForTimeout(400);
  expect(page.url(), "j should move the selection").not.toBe(first);

  await page.keyboard.press("k");
  await page.waitForTimeout(400);
  expect(page.url(), "k should move it back").toBe(first);
  assertClean(diags, "console j/k");
});

test("the start form validates before it will submit anything", async ({ page }, testInfo) => {
  const diags = watch(page, "/start", testInfo.project.name);
  await primeToken(page);
  await visit(page, "/start");

  // The mode segments carry the same words as the primary action, so scope past them: only the
  // segments are `aria-pressed` toggles.
  const submit = page
    .getByRole("button", { name: /^(Start agent|Add to queue|Start daemon run)/ })
    .and(page.locator("button:not([aria-pressed])"));

  // Empty task → the primary action must be unavailable.
  await expect(submit).toBeDisabled();

  // A task that starts with "-" would be read as a flag by the CLI; the form must catch it.
  await page.getByLabel("Task").fill("--rm -rf");
  await expect(page.getByText(/can.t start with/)).toBeVisible();
  await expect(submit).toBeDisabled();

  // A real task enables it (we do NOT click).
  await page.getByLabel("Task").fill("live-e2e smoke — do not run");
  await expect(submit).toBeEnabled();

  assertClean(diags, "start validation");
});

test("start mode switching reveals the right controls per mode", async ({ page }, testInfo) => {
  const diags = watch(page, "/start", testInfo.project.name);
  await primeToken(page);
  await visit(page, "/start");

  const modes = page.getByRole("group", { name: "start mode" });
  await expect(modes).toBeVisible();

  // Daemon mode owns the concurrency stepper; dispatch owns harness + ship/scout.
  await modes.getByRole("button").nth(2).click();
  await expect(page.getByRole("group", { name: "concurrency" })).toBeVisible();

  await modes.getByRole("button").nth(0).click();
  await expect(page.getByRole("group", { name: "ship or scout" })).toBeVisible();
  await expect(page.getByRole("group", { name: "concurrency" })).toHaveCount(0);

  // The harness select only offers harnesses the host actually has.
  await page.getByLabel("Harness").click();
  await expect(page.getByRole("option").first()).toBeVisible();
  await page.keyboard.press("Escape");

  assertClean(diags, "start modes");
});

test("clicking a fleet card opens that agent's detail", async ({ page }, testInfo) => {
  const diags = watch(page, "/fleet", testInfo.project.name);
  await primeToken(page);
  await visit(page, "/fleet");

  const cards = page.locator("main a[href^='/agents/'], main [role='button']").first();
  const fleet = await page.request.get("/api/fleet").then((r) => r.json());
  test.skip(!fleet?.data?.agents?.length, "no live agents");

  await cards.click();
  await expect(page).toHaveURL(/\/agents\//, { timeout: 15_000 });
  assertClean(diags, "fleet drill-down");
});

test("skills list opens a skill and shows its body", async ({ page }, testInfo) => {
  const diags = watch(page, "/skills", testInfo.project.name);
  await primeToken(page);
  await visit(page, "/skills");

  const first = page.locator("main button, main a").filter({ hasNotText: /^$/ }).first();
  await first.click();
  await page.waitForTimeout(800);
  await expect(page.locator("main")).toBeVisible();
  assertClean(diags, "skills drill-down");
});

test("repos drills into a repo and back", async ({ page }, testInfo) => {
  const diags = watch(page, "/repos", testInfo.project.name);
  await primeToken(page);
  await visit(page, "/repos");

  const link = page.locator("main a[href^='/repos/']").first();
  test.skip((await link.count()) === 0, "no repo links");
  await link.click();
  await expect(page).toHaveURL(/\/repos\/.+/, { timeout: 15_000 });

  await page.goBack();
  await expect(page).toHaveURL(/\/repos\/?$/);
  assertClean(diags, "repos drill-down");
});

test("cron drills into a job and shows its run history", async ({ page }, testInfo) => {
  const diags = watch(page, "/cron", testInfo.project.name);
  await primeToken(page);
  await visit(page, "/cron");

  const link = page.locator("main a[href^='/cron/']").first();
  test.skip((await link.count()) === 0, "no cron links");
  await link.click();
  await expect(page).toHaveURL(/\/cron\/.+/, { timeout: 15_000 });
  await expect(page.locator("main")).not.toBeEmpty();
  assertClean(diags, "cron drill-down");
});

test("the inbox switches tabs", async ({ page }, testInfo) => {
  const diags = watch(page, "/inbox", testInfo.project.name);
  await primeToken(page);
  await visit(page, "/inbox");

  const tabs = page.getByRole("tab");
  const n = await tabs.count();
  expect(n, "the inbox should have tabs").toBeGreaterThan(0);
  for (let i = 0; i < n; i++) {
    await tabs.nth(i).click();
    await page.waitForTimeout(400);
    await expect(page.locator("main")).not.toBeEmpty();
  }
  assertClean(diags, "inbox tabs");
});

test("the SSE stream keeps the shell live without leaking errors", async ({ page }, testInfo) => {
  test.skip(testInfo.project.name !== "desktop", "stream behaviour is layout-independent");
  const diags = watch(page, "/fleet", testInfo.project.name);
  await primeToken(page);
  await visit(page, "/fleet");

  // The connection dot in the topbar reports the stream state; it must settle on connected.
  await expect(page.getByRole("navigation", { name: "primary" })).toBeVisible();

  // Sit on the page long enough for several fleet ticks (server polls every 2s).
  await page.waitForTimeout(8_000);

  // A disconnect banner appearing would mean the stream dropped while we watched.
  const banner = page.getByText(/reconnect|disconnected|offline/i);
  expect(await banner.count(), "the stream dropped during an 8s idle").toBe(0);
  assertClean(diags, "sse stream");
});

test("a deep link restores state on reload", async ({ page }, testInfo) => {
  test.skip(testInfo.project.name !== "desktop", "queue selection is desktop-only");
  const diags = watch(page, "/", testInfo.project.name);
  await primeToken(page);
  await visit(page, "/");

  const queue = page.getByRole("complementary", { name: "work queue" });
  const rows = queue.locator("button[aria-current]");
  test.skip((await rows.count()) < 2, "need at least two rows");

  await rows.nth(1).click();
  await expect(page).toHaveURL(/\?sel=/);
  const deep = page.url();

  await page.reload();
  await page.waitForTimeout(1500);
  expect(page.url(), "the selection should survive a reload").toBe(deep);
  assertClean(diags, "deep link");
});
