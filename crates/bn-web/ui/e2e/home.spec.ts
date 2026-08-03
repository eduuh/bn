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

  // The three things that decide what happens next.
  await expect(page.getByRole("heading", { name: /^Tasks/ })).toBeVisible();
  await expect(page.getByRole("heading", { name: "Needs you" })).toBeVisible();
  await expect(page.getByRole("heading", { name: /Issues/ })).toBeVisible();

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
