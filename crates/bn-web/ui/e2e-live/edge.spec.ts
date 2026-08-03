// Edge cases: what the dashboard does when a URL points at something that isn't there.
//
// These are the paths a stale PWA shortcut, a shared link, or a renamed cron actually takes. The
// bar is low and non-negotiable: never a blank screen, always a way back into the shell.

import { expect, test } from "@playwright/test";
import { primeToken, shot, visit } from "./live";

const BOGUS = [
  { path: "/agents/00000000-dead-beef-0000-000000000000", name: "agent" },
  { path: "/repos/no-such-repo-at-all", name: "repo" },
  { path: "/cron/no-such-job-at-all", name: "cron" },
];

for (const c of BOGUS) {
  test(`an unknown ${c.name} id degrades gracefully`, async ({ page }, testInfo) => {
    test.skip(testInfo.project.name !== "desktop", "layout-independent");
    await primeToken(page);
    await visit(page, c.path);

    // The shell must survive so the operator can navigate away.
    await expect(
      page.getByRole("navigation", { name: "primary" }),
      `the shell vanished on ${c.path}`,
    ).toBeVisible();

    // And the screen must say *something* — a silent blank is indistinguishable from a hang.
    const main = page.locator("main");
    const text = (await main.innerText()).trim();
    expect(text.length, `${c.path} rendered an empty main`).toBeGreaterThan(0);

    await shot(page, testInfo, `bogus-${c.name}`);
  });
}

/**
 * The SPA used to declare no catch-all route and no `errorElement`, so React Router fell through to
 * its own dev-facing boundary: a bare "Unexpected Application Error! 404 Not Found" on a blank page,
 * with no nav, no branding and no link home. Any stale bookmark, renamed route or mistyped path
 * landed there — and because the same boundary catches *render* errors, one throwing component took
 * the whole dashboard down the same way.
 */
test("an unknown route renders a 404 inside the shell", async ({ page }, testInfo) => {
  test.skip(testInfo.project.name !== "desktop", "layout-independent");
  await primeToken(page);
  await visit(page, "/definitely-not-a-route");
  await shot(page, testInfo, "unknown-route");

  await expect(
    page.getByRole("navigation", { name: "primary" }),
    "an unknown route should render a 404 inside the shell, not React Router's raw error page",
  ).toBeVisible();

  // A real screen, not framework text — and a way back that does not rely on the browser button.
  await expect(page.getByText("Nothing lives here")).toBeVisible();
  await expect(page.getByText(/Unexpected Application Error/)).toHaveCount(0);

  // Landing on the console, which auto-selects the most-blocked row and reflects that in `?sel=`.
  await page.getByRole("link", { name: /Back to the console/ }).click();
  await expect(page).toHaveURL(/^[^?]*\/(\?|$)/);
  await expect(page.getByText("Nothing lives here")).toHaveCount(0);
});

test("a deep unknown path under a real prefix also keeps the shell", async ({ page }, testInfo) => {
  test.skip(testInfo.project.name !== "desktop", "layout-independent");
  await primeToken(page);
  await visit(page, "/cron/some/deeply/nested/nonsense");
  await expect(page.getByRole("navigation", { name: "primary" })).toBeVisible();
  await expect(page.getByText(/Unexpected Application Error/)).toHaveCount(0);
});

test("the API 404s unknown endpoints as JSON rather than serving the SPA shell", async ({
  page,
}, testInfo) => {
  test.skip(testInfo.project.name !== "desktop", "layout-independent");
  const res = await page.request.get("/api/no-such-endpoint");
  expect(res.status()).toBe(404);
  expect(await res.text()).not.toContain("<!doctype html>");
});

test("a token-less browser can still read and write (loopback trust model)", async ({
  page,
}, testInfo) => {
  test.skip(testInfo.project.name !== "desktop", "layout-independent");

  // No `primeToken` here: bn-web deliberately has no token gate (#523), trusting the bind address
  // instead. This test pins that decision so it cannot change silently — if a gate is ever added,
  // this fails and the UI's pairing flow needs to come back with it.
  await visit(page, "/fleet");
  await expect(page.getByRole("heading", { name: /Fleet/ }).first()).toBeVisible();

  const res = await page.request.post("/api/cron/run", { data: { job: "!!invalid!!" } });
  expect(
    res.status(),
    "an unauthenticated write should reach validation (400), not be rejected as unauthorized (401)",
  ).toBe(400);
});
