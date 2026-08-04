// Regression cover for the defects the first live pass turned up.
//
// Each test here names the behaviour that was wrong and asserts the corrected one, so the specific
// failure cannot come back quietly. They run against real state, which is the point: three of these
// were invisible to the fixture suite because they only appear against data nobody would seed (a
// Claude-only cost day, a session started outside bn, two agents on one branch).

import { expect, test } from "@playwright/test";
import { primeToken, shot, visit } from "./live";

/**
 * The steer panel used to render fully enabled for every agent in the fleet. But supervision
 * *observes* every running session on the machine, including ones bn never launched — for those
 * `POST /send` fails with "no such tmux window, or tmux absent" only *after* you click. Worse, two
 * sessions on one branch are indistinguishable to every write channel (all keyed `repo:branch`), so
 * a kill could land on either.
 *
 * `/api/fleet` now reports a `control` channel per agent and the panel refuses up front.
 */
test("the steer panel refuses agents bn cannot address, with a reason", async ({
  page,
}, testInfo) => {
  await primeToken(page);

  const fleet = await page.request.get("/api/fleet").then((r) => r.json());
  const agents: { session_id: string; control?: string; target: string }[] =
    fleet?.data?.agents ?? [];
  test.skip(!agents.length, "no live agents");

  // Every agent must carry the field — an unstamped fleet would silently re-enable the old
  // fail-after-click behaviour.
  for (const a of agents) {
    expect(a.control, `agent ${a.session_id} has no control channel`).toBeTruthy();
    expect(["resume", "tmux", "ambiguous", "none"]).toContain(a.control);
  }

  const blocked = agents.find((a) => a.control === "none" || a.control === "ambiguous");
  test.skip(!blocked, "every live agent is addressable right now");

  await visit(page, `/agents/${blocked!.session_id}`);

  // Not every fleet entry has a steer panel to gate: a just-dispatched "starting…" agent has a
  // launch record but no transcript, so its detail renders the not-found card instead. Wait for the
  // panel and bail out cleanly rather than failing on an agent this test isn't about.
  const panel = page.getByRole("heading", { name: "Steer" });
  const hasPanel = await panel
    .waitFor({ state: "visible", timeout: 10_000 })
    .then(() => true)
    .catch(() => false);
  test.skip(!hasPanel, `no steer panel on ${blocked!.session_id} — nothing to gate`);

  // Re-read after navigating. The fleet is live — a duplicate session can finish mid-test, which
  // flips its sibling from `ambiguous` to addressable — so asserting against the snapshot taken
  // before navigation races the SSE frame that redrew the panel. The real invariant is that the
  // panel agrees with whatever the server reports *now*.
  const now = await page.request.get("/api/fleet").then((r) => r.json());
  const current = (now?.data?.agents ?? []).find(
    (a: { session_id: string }) => a.session_id === blocked!.session_id,
  );
  test.skip(!current, "the agent finished mid-test");

  if (current.control === "none" || current.control === "ambiguous") {
    await expect(
      page.getByRole("status").filter({ hasText: /No live checkout|shares this branch/ }),
    ).toBeVisible();
    // Inert, not merely annotated.
    await expect(page.getByPlaceholder(/Not steerable from here/)).toBeDisabled();
    await expect(page.getByRole("button", { name: /^Send$/ })).toBeDisabled();
    await expect(page.getByRole("button", { name: /^Relaunch$/ })).toBeDisabled();
    await expect(page.getByRole("button", { name: /Kill/ })).toBeDisabled();
    await shot(page, testInfo, "steer-blocked");
  } else {
    // It became addressable while we watched — then the panel must have opened up to match.
    await expect(page.getByPlaceholder(/Message the agent/)).toBeEnabled();
    await expect(
      page.getByRole("status").filter({ hasText: /No live checkout|shares this branch/ }),
    ).toHaveCount(0);
  }
});

test("an addressable agent still gets a working steer panel", async ({ page }, testInfo) => {
  test.skip(testInfo.project.name !== "desktop", "layout-independent");
  await primeToken(page);

  const fleet = await page.request.get("/api/fleet").then((r) => r.json());
  const ok = (fleet?.data?.agents ?? []).find(
    (a: { control?: string }) => a.control === "resume" || a.control === "tmux",
  );
  test.skip(!ok, "no addressable agent right now — the gating test covers the other direction");

  await visit(page, `/agents/${ok.session_id}`);
  await expect(page.getByPlaceholder(/Message the agent/)).toBeEnabled();
  // Send stays disabled until there is a message, which is the pre-existing behaviour.
  await page.getByPlaceholder(/Message the agent/).fill("noop");
  await expect(page.getByRole("button", { name: /^Send$/ })).toBeEnabled();
});

/**
 * `bn agent recover` exits 0 whether or not it recovered anything, reporting the count in
 * `data.candidates`. Passed through, that made the dashboard toast a green "relaunch triggered"
 * while the stuck agent stayed stuck — a success message for a no-op, which ends the operator's
 * investigation exactly when it should start.
 */
test("relaunching something unrecoverable reports failure, not success", async ({
  page,
}, testInfo) => {
  test.skip(testInfo.project.name !== "desktop", "API-level, layout-independent");

  const res = await page.request.post("/api/agents/bn%3Ano-such-branch-at-all/relaunch", {
    data: { hard: false },
  });
  const body = await res.json();

  expect(body.ok, "a relaunch that recovered nothing must not report success").toBe(false);
  expect(body.error?.code).toBe("nothing_to_recover");
});

/**
 * The Today tile read `—` on any Claude-only day, because AIC is the *Copilot* meter — so most days
 * the screen whose job is "what did today cost" answered nothing, while the same cost row carried a
 * perfectly good API-equivalent figure.
 */
test("Today always answers what the day cost", async ({ page }, testInfo) => {
  await primeToken(page);

  const [wrap, cost] = await Promise.all([
    page.request.get("/api/wrap").then((r) => r.json()),
    page.request.get("/api/cost").then((r) => r.json()),
  ]);
  const today = (cost?.data?.by_day ?? []).find(
    (r: { key: string }) => r.key === wrap?.data?.date,
  );
  test.skip(!today, "no cost row for today yet");
  test.skip(
    today.aic === null && today.cost_usd === null,
    "nothing priceable ran today — a dash is the honest answer",
  );

  await visit(page, "/today");

  const tile = page
    .locator("main")
    .locator("div")
    .filter({ hasText: /^(AIC spent|\$ spent \(API-equiv\))/ })
    .first();
  await expect(tile).toBeVisible();
  await expect(tile, "the spend tile must show a figure, not a dash").not.toHaveText(/—/);

  await shot(page, testInfo, "today-spend");
});

/**
 * `fmtAic`/`fmtUsd` were `toFixed(2)` with no grouping, so the two headline figures on Cost — the
 * most important numbers on the page — rendered as `332589.65` and `$10161.70` while the token
 * counts beside them compacted properly to `35.22M`.
 */
test("headline money is grouped and readable", async ({ page }, testInfo) => {
  test.skip(testInfo.project.name !== "desktop", "layout-independent");
  await primeToken(page);

  const cost = await page.request.get("/api/cost").then((r) => r.json());
  const totals = cost?.data?.totals ?? {};
  test.skip(
    !(totals.cost_usd >= 1000 || totals.aic >= 1000),
    "nothing four-figure to group on this host",
  );

  await visit(page, "/insights?tab=cost");
  const main = page.locator("main");
  const text = await main.innerText();

  // At least one grouped figure must be on the page, and no bare six-plus-digit run of digits
  // before a decimal point should survive.
  expect(text, "expected a grouped figure like 10,161.70").toMatch(/\d{1,3}(,\d{3})+\.\d{2}/);
  expect(text, "found an ungrouped four-plus-digit money figure").not.toMatch(/(?<![\d,.])\d{5,}\.\d{2}/);

  await shot(page, testInfo, "cost-grouped");
});
