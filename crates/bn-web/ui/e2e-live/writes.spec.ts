// Write flows against the live bn-web.
//
// These mutate the owner's real state, so the suite is split by blast radius:
//
//   * **safe tier** (always runs) — writes that are inert, self-reverting, or that fire a job the
//     scheduler already runs on its own every few minutes. Nothing here costs tokens or touches a
//     running agent.
//   * **destructive tier** (`BN_LIVE_DESTRUCTIVE=1`) — dispatching real agents, steering, killing,
//     landing. Off by default: the live fleet contains the owner's actual work, and one of those
//     agents is the session running this suite.
//
// Every write is round-tripped: assert the effect landed, then put the state back.

import { expect, test } from "@playwright/test";
import { primeToken, recordDiagnostics, visit, watch } from "./live";

const DESTRUCTIVE = process.env.BN_LIVE_DESTRUCTIVE === "1";

/** A marker no real task would ever carry, so a leaked item is obvious and greppable. */
const MARKER = "zz-live-e2e-probe-delete-me";

test.describe("safe writes", () => {
  test("the write API rejects malformed input instead of 500ing", async ({ page }) => {
    // Path traversal in a repo/job segment must be refused at validation, not reach the shell.
    const cases: { url: string; body: unknown; code: string }[] = [
      { url: "/api/dispatch", body: { repo: "../../etc", items: ["x"] }, code: "invalid_repo" },
      { url: "/api/cron/run", body: { job: "../etc/passwd" }, code: "invalid_job" },
      { url: "/api/dispatch", body: {}, code: "invalid_repo" },
    ];

    for (const c of cases) {
      const res = await page.request.post(c.url, { data: c.body });
      expect(res.status(), `${c.url} should be a clean 4xx`).toBeGreaterThanOrEqual(400);
      expect(res.status(), `${c.url} must not 5xx`).toBeLessThan(500);
      const body = await res.json();
      expect(body.ok).toBe(false);
      expect(body.error?.code).toBe(c.code);
    }
  });

  test("garbage JSON is refused without crashing the server", async ({ page }) => {
    const res = await page.request.post("/api/dispatch", {
      headers: { "content-type": "application/json" },
      data: "{not json at all",
    });
    expect(res.status()).toBeGreaterThanOrEqual(400);
    expect(res.status()).toBeLessThan(500);

    // The server is still serving afterwards.
    const health = await page.request.get("/api/health");
    expect(health.ok()).toBe(true);
  });

  test("queue add → visible → remove round-trips", async ({ page }, testInfo) => {
    test.skip(testInfo.project.name !== "desktop", "API round-trip, layout-independent");

    // Only safe while no daemon run is live — otherwise the next tick would actually dispatch it.
    const orch = await page.request.get("/api/orchestrate").then((r) => r.json());
    test.skip(
      (orch?.data?.runs ?? []).length > 0,
      "a daemon run is live; queueing would really dispatch",
    );

    const add = await page.request.post("/api/orchestrate/queue", {
      data: { repo: "bn", items: [MARKER] },
    });
    const addBody = await add.json();

    // With no live run there is nothing to append to — a clean refusal is the correct behaviour,
    // and is itself the assertion. If it did land, remove it again.
    if (!addBody.ok) {
      expect(add.status()).toBeLessThan(500);
      expect(addBody.error?.code, `queue add failed with: ${JSON.stringify(addBody.error)}`)
        .toBeTruthy();
      return;
    }

    const id = addBody.data?.added?.[0]?.id ?? addBody.data?.id;
    expect(id, "a successful queue add should return the item id").toBeTruthy();

    const remove = await page.request.delete(`/api/orchestrate/queue/${id}?repo=bn`);
    expect(remove.ok(), "the probe item must be removable again").toBe(true);

    const after = await page.request.get("/api/orchestrate").then((r) => r.json());
    expect(JSON.stringify(after)).not.toContain(MARKER);
  });

  test("a skill edit round-trips through the UI's own endpoint", async ({ page }, testInfo) => {
    test.skip(testInfo.project.name !== "desktop", "API round-trip, layout-independent");

    const skills = await page.request.get("/api/skills").then((r) => r.json());
    const name: string | undefined = (skills?.data?.skills ?? [])[0]?.name;
    test.skip(!name, "no skills discovered on this host");

    const before = await page.request.get(`/api/skills/${name}`).then((r) => r.json());
    const content: string | undefined = before?.data?.content ?? before?.data?.source;
    test.skip(!content, "skill detail carries no editable source");

    // Write the byte-identical source back: proves the validate → persist path works end to end
    // while leaving the file exactly as it was.
    const put = await page.request.put(`/api/skills/${name}`, { data: { content } });
    expect(put.ok(), `writing a skill back unchanged should succeed: ${await put.text()}`).toBe(true);

    const after = await page.request.get(`/api/skills/${name}`).then((r) => r.json());
    expect(after?.data?.content ?? after?.data?.source).toBe(content);
  });

  test("a malformed skill edit is rejected and leaves the file intact", async ({
    page,
  }, testInfo) => {
    test.skip(testInfo.project.name !== "desktop", "API round-trip, layout-independent");

    const skills = await page.request.get("/api/skills").then((r) => r.json());
    const name: string | undefined = (skills?.data?.skills ?? [])[0]?.name;
    test.skip(!name, "no skills discovered on this host");

    const before = await page.request.get(`/api/skills/${name}`).then((r) => r.json());
    const content: string | undefined = before?.data?.content ?? before?.data?.source;
    test.skip(!content, "skill detail carries no editable source");

    // No frontmatter at all — must not parse as a skill.
    const put = await page.request.put(`/api/skills/${name}`, {
      data: { content: "this is not a skill file" },
    });
    expect(put.status(), "a corrupt skill edit must be refused").toBeGreaterThanOrEqual(400);

    const after = await page.request.get(`/api/skills/${name}`).then((r) => r.json());
    expect(
      after?.data?.content ?? after?.data?.source,
      "a rejected edit must not have touched the file",
    ).toBe(content);
  });

  test("re-running an idempotent tick cron from the UI reports an outcome", async ({
    page,
  }, testInfo) => {
    test.skip(testInfo.project.name !== "desktop", "one run is enough");
    const diags = watch(page, "/cron", testInfo.project.name);
    await primeToken(page);
    await visit(page, "/cron");

    // `attention-sync` just refreshes a cache and the scheduler already fires it every 5 minutes,
    // so an extra manual run is indistinguishable from normal operation.
    const job = "attention-sync";
    const card = page.getByRole("article", { name: `cron ${job}` });
    await expect(card).toBeVisible();

    // Every re-run is gated behind a native `window.confirm`, which Playwright auto-dismisses —
    // accept it, or the click is silently a no-op.
    page.once("dialog", (d) => void d.accept());

    const before = await page.request.get("/api/cron").then((r) => r.json());
    const runsBefore =
      (before?.data?.jobs ?? []).find((j: { job: string }) => j.job === job)?.runs ?? 0;

    await card.getByRole("button", { name: /re-run/i }).click();

    // The run is fire-and-forget; the card must acknowledge rather than sit silent.
    await page.waitForTimeout(6_000);
    const after = await page.request.get("/api/cron").then((r) => r.json());
    const runsAfter =
      (after?.data?.jobs ?? []).find((j: { job: string }) => j.job === job)?.runs ?? 0;

    expect(runsAfter, `${job} should have recorded another run`).toBeGreaterThan(runsBefore);
    recordDiagnostics(diags);
    expect(diags.filter((d) => d.kind === "pageerror")).toEqual([]);
  });

  test("warming a repo's worktree pool is accepted", async ({ page }, testInfo) => {
    test.skip(testInfo.project.name !== "desktop", "API round-trip, layout-independent");
    // Warming only materializes pre-built worktrees — no agent runs, no tokens spent.
    const res = await page.request.post("/api/repos/bn/pool/warm");
    const body = await res.json();
    // Either it warms, or it cleanly reports the repo is not pooled. A 5xx is the only failure.
    expect(res.status(), `pool warm returned: ${JSON.stringify(body)}`).toBeLessThan(500);
    expect(body).toHaveProperty("ok");
  });
});

test.describe("destructive writes", () => {
  test.skip(!DESTRUCTIVE, "set BN_LIVE_DESTRUCTIVE=1 to run — these spend tokens and touch agents");

  test("dispatch starts a real agent and it appears on the fleet", async ({ page }, testInfo) => {
    test.skip(testInfo.project.name !== "desktop", "one dispatch is enough");
    const diags = watch(page, "/start", testInfo.project.name);
    await primeToken(page);
    await visit(page, "/start");

    await page.getByLabel("Task").fill(`${MARKER}: print the repo name and stop`);
    await page.getByLabel("Repo").fill("bn");

    const submit = page
      .getByRole("button", { name: /^Start agent/ })
      .and(page.locator("button:not([aria-pressed])"));
    await expect(submit).toBeEnabled();
    await submit.click();

    // The dashboard must confirm rather than leaving the operator guessing.
    await expect(page.getByText(/dispatch|started|queued/i).first()).toBeVisible({
      timeout: 20_000,
    });

    recordDiagnostics(diags);
    expect(diags.filter((d) => d.kind === "pageerror")).toEqual([]);
  });

  test("steering a running agent delivers the message", async ({ page }, testInfo) => {
    test.skip(testInfo.project.name !== "desktop", "one steer is enough");
    const fleet = await page.request.get("/api/fleet").then((r) => r.json());
    const target = (fleet?.data?.agents ?? []).find((a: { target: string }) =>
      a.target.includes(MARKER.slice(0, 12)),
    );
    test.skip(!target, "no probe agent to steer — only steer an agent this suite started");

    const res = await page.request.post(`/api/agents/${encodeURIComponent(target.target)}/send`, {
      data: { message: "wrap up now" },
    });
    expect(res.ok()).toBe(true);
  });
});
