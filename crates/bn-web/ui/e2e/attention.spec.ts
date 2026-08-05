import { expect, test } from "@playwright/test";
import { seed } from "./fixtures";
import { primeToken, shot } from "./helpers";

test.beforeEach(() => seed());

test("attention list renders items and acking one persists on the server", async ({
  page,
}, testInfo) => {
  await primeToken(page);
  await page.goto("/attention");

  await expect(page.getByRole("heading", { name: "Attention", exact: true })).toBeVisible();

  const urgent = page.getByRole("article", {
    name: "urgent attention: Prod dashboard is 500ing",
  });
  await expect(urgent).toBeVisible();
  await expect(urgent.getByText("urgent", { exact: true })).toBeVisible();
  await expect(page.getByText(/PR #481 has merge conflicts/)).toBeVisible();

  await shot(page, testInfo, "attention");

  // Ack the urgent item — a real POST /api/attention/a1 {action:"ack"}.
  const [req] = await Promise.all([
    page.waitForRequest(
      (r) => r.url().includes("/api/attention/a1") && r.method() === "POST",
    ),
    urgent.getByRole("button", { name: "Ack" }).click(),
  ]);
  expect(JSON.parse(req.postData() ?? "{}").action).toBe("ack");
  await expect(page.getByText(/Acked a1/)).toBeVisible();

  // Persisted: after a reload the server no longer reports the acked item.
  await page.reload();
  await expect(
    page.getByRole("article", { name: "urgent attention: Prod dashboard is 500ing" }),
  ).toHaveCount(0);
});
