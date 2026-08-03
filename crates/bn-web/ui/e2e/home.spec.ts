// The task-first home (#1006) — `/` is now Today, not the console.
//
// The load-bearing assertion here is the *absence* of "Start an agent". That button was the whole
// problem the epic exists to fix: it spawned work from a free-text prompt and left no durable
// record, so when the agent died the intent died with it. If it ever comes back, this fails.
import { expect, test } from "@playwright/test";
import { seed } from "./fixtures";

test.beforeEach(async ({ page }) => {
  await seed(page);
});

test("the index is the task-first home, and work starts from a task", async ({ page }) => {
  await page.goto("/");

  await expect(page.getByRole("heading", { name: "Today", level: 1 })).toBeVisible();

  // The lifecycle, one list per state (#1033). The single undifferentiated "Tasks" list is gone:
  // it mixed work that could be started with work that had not earned the right to be.
  await expect(page.getByRole("heading", { name: "Ready to start" })).toBeVisible();
  await expect(page.getByRole("heading", { name: /^In flight/ })).toBeVisible();
  await expect(page.getByRole("heading", { name: "Untriaged" })).toBeVisible();
  await expect(page.getByRole("heading", { name: "Done today" })).toBeVisible();
  await expect(page.getByRole("heading", { name: "Needs you" })).toBeVisible();
  await expect(page.getByRole("heading", { name: /Issues/ })).toBeVisible();

  // Triage is a top-level action, not buried in the issues card: the gate can deadlock the day,
  // so the way out of "nothing is startable" has to be reachable from the blocked state.
  await expect(
    page.getByRole("button", { name: "Triage for today" }).first(),
  ).toBeVisible();

  // The old verb is gone, everywhere on the page.
  await expect(page.getByText("Start an agent", { exact: false })).toHaveCount(0);
  await expect(page.getByRole("link", { name: "New task" })).toBeVisible();
});

test("the pipeline is drawn, with every stage present", async ({ page }) => {
  await page.goto("/");

  const pipeline = page.getByRole("region", { name: "pipeline" });
  await expect(pipeline).toBeVisible();

  // All nine stages, including the empty ones — a stage with nothing in it is a fact about the
  // pipeline, and hiding it would make the flow read as shorter than it is.
  for (const stage of [
    "pickup",
    "investigate",
    "plan",
    "consensus",
    "execute",
    "validate",
    "critique",
    "ship",
    "learn",
  ]) {
    await expect(pipeline.getByLabel(new RegExp(`^${stage}: `))).toBeVisible();
  }
});

test("selecting a stage drills into that stage's work", async ({ page }) => {
  await page.goto("/");

  const pipeline = page.getByRole("region", { name: "pipeline" });
  await pipeline.getByLabel(/^execute: /).click();

  // The section retitles to the stage, and a "clear filter" escape appears. The bug this replaces:
  // the node advertised work while the list below it said there was none, because the filter ran
  // against `task.stage` — a field nothing has ever written.
  await expect(page.getByRole("heading", { name: "In flight · execute" })).toBeVisible();
  await expect(page.getByRole("button", { name: "clear filter" })).toBeVisible();

  await page.getByRole("button", { name: "clear filter" }).click();
  await expect(page.getByRole("heading", { name: "In flight", exact: true })).toBeVisible();
});

test("the console still exists, at its own route", async ({ page }) => {
  await page.goto("/console");
  await expect(page.getByRole("complementary", { name: "work queue" })).toBeVisible();
});

test("a task can be created from the home page", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("button", { name: "New task" }).click();

  const dialog = page.getByRole("dialog");
  await expect(dialog).toBeVisible();
  // Only the title is required — a rough task written down beats a good one lost.
  await expect(dialog.getByRole("button", { name: "Create" })).toBeDisabled();

  await dialog.getByPlaceholder(/Fix the SSE reconnect storm/).fill("e2e created task");
  await expect(dialog.getByRole("button", { name: "Create" })).toBeEnabled();
});
