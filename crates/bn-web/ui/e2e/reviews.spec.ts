import { expect, test } from "@playwright/test";
import { seed } from "./fixtures";
import { primeToken, shot } from "./helpers";

test.beforeEach(() => seed());

test("approve a pending plan review — persists on the server", async ({ page }, testInfo) => {
  await primeToken(page);
  await page.goto("/reviews");

  await expect(page.getByRole("tab", { name: /Reviews/ })).toHaveAttribute(
    "data-state",
    "active",
  );
  await expect(page.getByRole("heading", { name: "Pending" })).toBeVisible();
  await expect(page.getByText(/surface cost in the fleet cards/)).toBeVisible();

  await shot(page, testInfo, "reviews");

  const pending = page.getByRole("region", { name: "Pending reviews" });
  const r1 = pending.getByRole("article", {
    name: "review r1 dispatch/plan-review-501",
  });
  await r1.getByRole("button", { name: "Approve" }).click();
  await expect(page.getByText(/Plan approved/)).toBeVisible();

  // Persisted: after a reload the server no longer reports r1 as pending.
  await page.reload();
  await expect(
    page
      .getByRole("region", { name: "Pending reviews" })
      .getByRole("article", { name: "review r1 dispatch/plan-review-501" }),
  ).toHaveCount(0);
  const approved = page
    .getByRole("region", { name: "Review history" })
    .getByRole("article", { name: "review r1 dispatch/plan-review-501" });
  await expect(approved.getByText("approved", { exact: true })).toBeVisible();
});

test("request changes requires notes and persists them", async ({ page }) => {
  await primeToken(page);
  await page.goto("/reviews");

  const r3 = page.getByRole("article", {
    name: "review r3 dispatch/refactor-cost-507",
  });
  await expect(r3.getByRole("button", { name: "Request changes" })).toBeDisabled();

  await r3.locator("textarea").fill("split into two PRs");
  await r3.getByRole("button", { name: "Request changes" }).click();
  await expect(page.getByText(/Changes requested/)).toBeVisible();

  await page.reload();
  await expect(page.getByText("notes: split into two PRs")).toBeVisible();
});
