// What the dashboard does when things go wrong underneath it.
//
// The route sweep proves each screen renders when the server is healthy and the data is well
// formed. That is the easy half. This spec breaks the assumptions on purpose — malformed payloads,
// a dead server, a slow one — because the dashboard runs on a LAN against a machine that reboots,
// and "the API went weird" is a normal Tuesday rather than an exotic edge case.
//
// Every fault is injected client-side with `page.route`, so the real server is never harmed and
// these are safe to run any time.

import { expect, test } from "@playwright/test";
import { primeToken, shot, visit } from "./live";

/** An envelope that is *shaped* right but carries nonsense where a component expects a collection. */
const POISON = JSON.stringify({
  ok: true,
  command: "cost",
  data: { totals: null, by_day: null, by_day_category: null, by_repo: null, window_days: 30 },
  error: null,
});

/**
 * The router now carries `RouteError` on a pathless layout route, so a screen that throws mid-render
 * should cost you that screen and nothing else. Until this test existed that guard was unproven —
 * the 404 path exercised the catch-all route, never the error boundary.
 *
 * Serving a well-formed envelope with null collections is the honest version of the fault: it is
 * exactly what a partially-written or version-skewed API returns, and it crashes on the first
 * `.map`.
 */
test("a screen that throws mid-render does not take the dashboard with it", async ({
  page,
}, testInfo) => {
  test.skip(testInfo.project.name !== "desktop", "layout-independent");
  await primeToken(page);

  await page.route("**/api/cost**", (route) =>
    route.fulfill({ status: 200, contentType: "application/json", body: POISON }),
  );

  await visit(page, "/insights?tab=cost");

  // The shell must survive so the operator can leave.
  await expect(
    page.getByRole("navigation", { name: "primary" }),
    "a crashing screen must not blank the shell",
  ).toBeVisible();

  // This payload really does throw (`Cannot read properties of null (reading 'map')`), so the
  // boundary itself is what we are asserting — not merely that the page looks non-blank.
  await expect(page.getByText("This screen failed to render")).toBeVisible();
  await expect(page.getByText(/Cannot read properties of null/)).toBeVisible();
  await expect(page.getByText(/Unexpected Application Error/)).toHaveCount(0);

  // The boundary offers a way out, not just an apology. Scoped to `main` — the rail carries its own
  // Console link, and matching that would prove nothing about the error card.
  const card = page.locator("main");
  await expect(card.getByRole("button", { name: /Reload/ })).toBeVisible();
  await expect(card.getByRole("link", { name: "Console" })).toBeVisible();

  await shot(page, testInfo, "poisoned-cost");

  // And the rest of the app still works — navigate away and land somewhere real.
  await page.getByRole("navigation", { name: "primary" }).getByRole("link", { name: /^Fleet/ }).click();
  await expect(page.getByRole("heading", { name: /Fleet/ }).first()).toBeVisible();
});

test("every screen survives its endpoint returning a well-formed but empty payload", async ({
  page,
}, testInfo) => {
  test.skip(testInfo.project.name !== "desktop", "layout-independent");
  await primeToken(page);

  // The empty state is the one a fresh install sees, and the one nobody tests because seeded
  // fixtures always have data.
  await page.route("**/api/**", async (route) => {
    const url = route.request().url();
    if (url.includes("/api/stream") || url.includes("/api/health")) return route.continue();
    const res = await route.fetch();
    const body = await res.json().catch(() => null);
    if (!body?.ok) return route.fulfill({ response: res });
    return route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({ ...body, data: emptied(body.data) }),
    });
  });

  for (const path of ["/", "/fleet", "/today", "/cron", "/goals", "/attention", "/repos", "/skills"]) {
    await visit(page, path);
    await expect(page.getByRole("navigation", { name: "primary" })).toBeVisible();
    await expect(page.getByText(/Unexpected Application Error/), `${path} crashed`).toHaveCount(0);
    const text = (await page.locator("main").innerText()).trim();
    expect(text.length, `${path} rendered nothing on an empty payload`).toBeGreaterThan(0);
  }

  await shot(page, testInfo, "empty-everything");
});

/** Recursively blank out collections while keeping the object's shape — a "nothing yet" server. */
function emptied(v: unknown): unknown {
  if (Array.isArray(v)) return [];
  if (v && typeof v === "object") {
    return Object.fromEntries(Object.entries(v).map(([k, val]) => [k, emptied(val)]));
  }
  return v;
}

test("losing the server surfaces a connection banner rather than silence", async ({
  page,
}, testInfo) => {
  test.skip(testInfo.project.name !== "desktop", "layout-independent");
  await primeToken(page);
  await visit(page, "/fleet");
  await expect(page.getByRole("heading", { name: /Fleet/ }).first()).toBeVisible();

  // Cut every API call, including the SSE stream, as if the host went away.
  await page.route("**/api/**", (route) => route.abort("connectionrefused"));

  // Force the app to notice: the stream drop plus a refetch on a fresh navigation.
  await page.getByRole("navigation", { name: "primary" }).getByRole("link", { name: /^Cost/ }).click();
  await page.waitForTimeout(6_000);

  // The shell must stay, and the operator must be told the data is stale — a dashboard that keeps
  // showing a confident-looking fleet from cache after the host died is actively misleading.
  await expect(page.getByRole("navigation", { name: "primary" })).toBeVisible();
  await shot(page, testInfo, "server-lost");

  const warned =
    (await page.getByText(/reconnect|disconnect|offline|lost|stale|unavailable|could not/i).count()) >
    0;
  expect(warned, "no indication at all that the server is gone").toBe(true);
});

test("a slow API shows loading affordances instead of an empty screen", async ({
  page,
}, testInfo) => {
  test.skip(testInfo.project.name !== "desktop", "layout-independent");
  await primeToken(page);

  await page.route("**/api/cost**", async (route) => {
    await new Promise((r) => setTimeout(r, 2_500));
    return route.continue();
  });

  await page.goto("/insights?tab=cost", { waitUntil: "domcontentloaded" });

  // While the query is in flight the screen must show *something* — the skeletons the route
  // already defines — rather than reading as an empty dataset.
  await expect(page.getByRole("navigation", { name: "primary" })).toBeVisible();
  await page.waitForTimeout(800);
  const midFlight = (await page.locator("main").innerText()).trim();
  expect(midFlight.length, "nothing on screen while cost loads").toBeGreaterThan(0);

  // And it resolves.
  await expect(page.getByText(/Usage by day/)).toBeVisible({ timeout: 20_000 });
});

test("a 500 from one endpoint does not blank the screen that uses it", async ({
  page,
}, testInfo) => {
  test.skip(testInfo.project.name !== "desktop", "layout-independent");
  await primeToken(page);

  await page.route("**/api/cost**", (route) =>
    route.fulfill({
      status: 500,
      contentType: "application/json",
      body: JSON.stringify({ ok: false, command: "cost", data: null, error: { code: "boom", message: "kaboom" } }),
    }),
  );

  await visit(page, "/insights?tab=cost");
  await expect(page.getByRole("navigation", { name: "primary" })).toBeVisible();
  // The cost panel declares an explicit error card — it must be what shows.
  await expect(page.getByText(/Could not load cost/i)).toBeVisible();
  await shot(page, testInfo, "cost-5xx");
});
