import { expect, test } from "@playwright/test";
import { seed } from "./fixtures";
import { shot } from "./helpers";

test.beforeEach(() => seed());

test("cron registry shows declared schedule + last-run health", async ({ page }, testInfo) => {
  await page.goto("/cron");

  await expect(page.getByRole("heading", { name: "Cron", exact: true })).toBeVisible();

  // A healthy declared job shows its OnCalendar schedule and an "ok" chip.
  const cleanup = page.getByRole("article", { name: "cron wt-cleanup" });
  await expect(cleanup).toBeVisible();
  await expect(cleanup.getByText("*:0/15")).toBeVisible();
  await expect(cleanup.getByText("ok", { exact: true })).toBeVisible();

  // A failed run surfaces its non-zero exit — and (failing-first) leads the list.
  const refresh = page.getByRole("article", { name: "cron refresh-all" });
  await expect(refresh.getByText("exit 1", { exact: true })).toBeVisible();

  await shot(page, testInfo, "cron");
});
