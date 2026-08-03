// Destructive write flows against the live fleet — explicitly authorized, and deliberately narrow.
//
// Run with: `BN_LIVE_DESTRUCTIVE=1 npx playwright test -c playwright.live.config.ts destructive.spec.ts`
//
// Two guard rails, because this drives the owner's real agents:
//
//   * **Never the driving session.** `BN_SELF_TARGET` names the agent running this suite; every
//     test that picks a target refuses to select it. Steering or relaunching ourselves would kill
//     the run mid-assert.
//   * **Prefer the stuck agent.** Steer and relaunch are aimed at an agent the fleet has already
//     given up on (`health: "stuck"`), so the feature is proven on the agent it exists to rescue
//     rather than by interrupting one doing real work. If nothing is stuck, the test skips instead
//     of picking a healthy victim.
//
// Every native `window.confirm` gate is accepted explicitly — Playwright auto-dismisses dialogs, so
// an unhandled one turns the click into a silent no-op that would pass for a broken feature.

import { expect, test } from "@playwright/test";
import { primeToken, recordDiagnostics, shot, visit, watch } from "./live";

test.skip(
  process.env.BN_LIVE_DESTRUCTIVE !== "1",
  "set BN_LIVE_DESTRUCTIVE=1 — these drive the owner's real agents",
);

/** The agent running this suite. Never a valid target. */
const SELF = process.env.BN_SELF_TARGET ?? "";

type Agent = {
  target: string;
  session_id: string;
  health: string;
  idle_secs: number;
  control?: string;
};

/**
 * Pick the stuck agent that is not us *and* that bn can actually address, or `null` if there is
 * nothing safe to act on. The `control` filter matters: the fleet observes sessions bn never
 * launched and cannot write to, and two sessions on one branch are indistinguishable to every write
 * channel — acting on either is a bug, not a test.
 */
async function stuckVictim(request: { get: (u: string) => Promise<{ json: () => Promise<unknown> }> }) {
  const fleet = (await request.get("/api/fleet").then((r) => r.json())) as {
    data?: { agents?: Agent[] };
  };
  const agents = fleet.data?.agents ?? [];
  return (
    agents.find(
      (a) =>
        a.health === "stuck" &&
        a.target !== SELF &&
        (a.control === "resume" || a.control === "tmux"),
    ) ?? null
  );
}

test("steering a stuck agent from the UI delivers the message", async ({ page }, testInfo) => {
  test.skip(testInfo.project.name !== "desktop", "one steer is enough");
  const diags = watch(page, "/agents/:id", testInfo.project.name);
  await primeToken(page);

  const victim = await stuckVictim(page.request);
  test.skip(!victim, "no stuck agent to rescue — refusing to steer a healthy one");
  expect(victim!.target, "refusing to steer the session running this suite").not.toBe(SELF);

  await visit(page, `/agents/${victim!.session_id}`);

  const box = page.getByPlaceholder(/Message the agent/);
  await expect(box).toBeVisible();
  await box.fill("status check from the live e2e suite — if you are stuck, say so and stop.");

  const send = page.getByRole("button", { name: /^Send$/ });
  await expect(send).toBeEnabled();

  // Assert on the response envelope, not on page text: a failed steer replies `200 {ok:false}`,
  // and any loose "sent|failed" text matcher scores that as a pass.
  const [res] = await Promise.all([
    page.waitForResponse((r) => r.url().includes("/send") && r.request().method() === "POST"),
    send.click(),
  ]);
  const body = await res.json();

  await shot(page, testInfo, "steer-sent");
  recordDiagnostics(diags);

  expect(
    body.ok,
    `steer failed: ${body.error?.code} — ${body.error?.message}. bn-web reaches agents through ` +
      `tmux, so a session started outside tmux cannot be steered, yet the panel still offers it.`,
  ).toBe(true);
});

test("relaunching a stuck agent runs the recovery path", async ({ page }, testInfo) => {
  test.skip(testInfo.project.name !== "desktop", "one relaunch is enough");
  const diags = watch(page, "/agents/:id", testInfo.project.name);
  await primeToken(page);

  const victim = await stuckVictim(page.request);
  test.skip(!victim, "no stuck agent to recover");
  expect(victim!.target, "refusing to relaunch the session running this suite").not.toBe(SELF);

  await visit(page, `/agents/${victim!.session_id}`);

  // Soft relaunch is `bn agent recover --yes` — the nudge a stuck agent exists to receive, and the
  // one button on this panel with no confirm gate.
  const relaunch = page.getByRole("button", { name: /^Relaunch$/ });
  await expect(relaunch).toBeVisible();

  const [res] = await Promise.all([
    page.waitForResponse((r) => r.url().includes("/relaunch") && r.request().method() === "POST"),
    relaunch.click(),
  ]);
  const body = await res.json();

  await shot(page, testInfo, "relaunch-sent");
  recordDiagnostics(diags);

  expect(body.ok, `relaunch errored: ${JSON.stringify(body.error)}`).toBe(true);

  // `bn agent recover` reports how many candidates it acted on. Zero means it recovered nothing —
  // but the envelope still says `ok:true`, so the UI toasts "relaunch triggered" either way. A
  // success toast for a no-op is worse than an error: it tells the operator the stuck agent is
  // being rescued when nothing happened.
  expect(
    body.data?.candidates ?? 0,
    "relaunch reported success while recovering 0 agents — the UI shows this as 'relaunch triggered'",
  ).toBeGreaterThan(0);
});

/**
 * "Land ready PRs" runs `bn wt merge --all`, which merges *every* open PR of yours for the repo —
 * and `--all` is documented as "the fast, ungated batch path", i.e. it skips the rebase-onto-main +
 * verify gate that a single `bn wt merge` applies by default.
 *
 * With zero open PRs this is a proven no-op, so it exercises the whole button → API → `bn` path
 * without merging anything.
 */
test("landing with nothing ready is a clean no-op", async ({ page }, testInfo) => {
  test.skip(testInfo.project.name !== "desktop", "one land is enough");

  const open = await page.request
    .get("https://api.github.com/repos/eduuh/bn/pulls?state=open")
    .then((r) => r.json())
    .catch(() => []);
  test.skip(
    Array.isArray(open) && open.length > 0,
    `${Array.isArray(open) ? open.length : "?"} PRs are open — landing would really merge them`,
  );

  const diags = watch(page, "/", testInfo.project.name);
  await primeToken(page);
  await visit(page, "/");

  const land = page.getByRole("button", { name: /Land ready PRs/ });
  await expect(land).toBeVisible();

  page.once("dialog", (d) => void d.accept());

  const [res] = await Promise.all([
    page.waitForResponse((r) => r.url().includes("/api/land") && r.request().method() === "POST", {
      timeout: 60_000,
    }),
    land.click(),
  ]);
  const body = await res.json();

  await shot(page, testInfo, "land-noop");
  recordDiagnostics(diags);

  expect(body.ok, `land errored: ${JSON.stringify(body.error)}`).toBe(true);
  expect(diags.filter((d) => d.kind === "pageerror")).toEqual([]);
});

/**
 * The plan-review happy path needs an agent parked awaiting approval, and there is none. Testing
 * the decision endpoint's failure path at least proves an approval for a vanished review is
 * refused cleanly rather than 500ing or silently succeeding.
 */
test("deciding a review that no longer exists is refused cleanly", async ({ page }, testInfo) => {
  test.skip(testInfo.project.name !== "desktop", "API-level, layout-independent");

  const reviews = await page.request.get("/api/reviews").then((r) => r.json());
  expect(reviews?.data?.reviews ?? [], "this test assumes an empty review inbox").toEqual([]);

  const res = await page.request.post("/api/reviews/no-such-review-id", {
    data: { decision: "approve" },
  });
  expect(res.status(), "must not 5xx").toBeLessThan(500);
  const body = await res.json();
  expect(body.ok).toBe(false);
  expect(body.error?.code).toBeTruthy();
});
