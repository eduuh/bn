// Route sweep against the live bn-web.
//
// One test per route per viewport. Each one proves three things that fixtures cannot:
//   1. the route renders against *real* data (a heading lands, the shell is intact),
//   2. nothing in it throws — no uncaught exception, no console error, no 4xx/5xx,
//   3. it looks right, via a committed full-page screenshot for design review.
//
// Deliberately data-agnostic: the live fleet changes minute to minute, so nothing here asserts a
// specific agent, cost or cron. Anything data-specific belongs in `e2e/` over fixtures.

import { expect, test } from "@playwright/test";
import { primeToken, recordDiagnostics, shot, visit, watch } from "./live";

/** Every destination the rail can reach, plus the two detail routes resolved at runtime. */
const ROUTES: { path: string; name: string; heading?: RegExp }[] = [
  { path: "/", name: "console" },
  { path: "/inbox", name: "inbox" },
  { path: "/today", name: "today", heading: /Today/ },
  { path: "/fleet", name: "fleet", heading: /Fleet/ },
  { path: "/nodes", name: "nodes", heading: /Nodes/ },
  { path: "/start", name: "start", heading: /Start/ },
  { path: "/cost", name: "cost", heading: /Cost/ },
  { path: "/insights", name: "insights", heading: /Insights/ },
  { path: "/notes", name: "notes", heading: /Notes/ },
  { path: "/handoff", name: "handoff", heading: /Handoff/ },
  { path: "/attention", name: "attention", heading: /Attention/ },
  { path: "/goals", name: "goals", heading: /Goals/ },
  { path: "/cron", name: "cron", heading: /Cron/ },
  { path: "/skills", name: "skills", heading: /Skills/ },
  { path: "/research", name: "research", heading: /Research/ },
  { path: "/repos", name: "repos", heading: /Repos/ },
];

for (const route of ROUTES) {
  test(`${route.name} renders clean`, async ({ page }, testInfo) => {
    const diags = watch(page, route.path, testInfo.project.name);
    await primeToken(page);
    await visit(page, route.path);

    // The shell must survive: the primary nav is present on every route, both layouts.
    await expect(page.getByRole("navigation", { name: "primary" })).toBeVisible();

    if (route.heading) {
      await expect(page.getByRole("heading", { name: route.heading }).first()).toBeVisible();
    }

    // A crashed React subtree leaves the region blank — catch the obvious version of that.
    const main = page.locator("main");
    await expect(main).toBeVisible();
    const text = (await main.innerText()).trim();
    expect(text.length, `main is empty on ${route.path}`).toBeGreaterThan(0);

    await shot(page, testInfo, route.name);

    recordDiagnostics(diags);
    const fatal = diags.filter((d) => d.kind !== "console");
    expect(fatal, `errors on ${route.path}:\n${fatal.map((d) => d.text).join("\n")}`).toEqual([]);
    const consoleErrors = diags.filter((d) => d.kind === "console" && d.text.startsWith("[error]"));
    expect(
      consoleErrors,
      `console errors on ${route.path}:\n${consoleErrors.map((d) => d.text).join("\n")}`,
    ).toEqual([]);
  });
}

test("agent detail renders for a real running agent", async ({ page }, testInfo) => {
  const diags = watch(page, "/agents/:id", testInfo.project.name);
  await primeToken(page);

  const fleet = await page.request.get("/api/fleet").then((r) => r.json());
  const agent = fleet?.data?.agents?.[0];
  test.skip(!agent, "no live agents to open");

  await visit(page, `/agents/${agent.session_id}`);
  await expect(page.locator("main")).toContainText(agent.branch);

  await shot(page, testInfo, "agent-detail");
  recordDiagnostics(diags);
  expect(diags.filter((d) => d.kind === "pageerror")).toEqual([]);
});

test("repo detail renders for a real managed repo", async ({ page }, testInfo) => {
  const diags = watch(page, "/repos/:repo", testInfo.project.name);
  await primeToken(page);

  const repos = await page.request.get("/api/repos").then((r) => r.json());
  const repo = repos?.data?.repos?.[0];
  test.skip(!repo, "no managed repos");

  await visit(page, `/repos/${repo.name ?? repo.repo}`);
  await expect(page.locator("main")).toBeVisible();

  await shot(page, testInfo, "repo-detail");
  recordDiagnostics(diags);
  expect(diags.filter((d) => d.kind === "pageerror")).toEqual([]);
});

test("cron detail renders for a real declared job", async ({ page }, testInfo) => {
  const diags = watch(page, "/cron/:id", testInfo.project.name);
  await primeToken(page);

  const cron = await page.request.get("/api/cron").then((r) => r.json());
  const job = cron?.data?.jobs?.[0];
  test.skip(!job?.job, "no declared crons");

  await visit(page, `/cron/${job.job}`);
  // The detail must actually be about the job we asked for, not a blank shell.
  await expect(page.locator("main")).toContainText(job.job);

  await shot(page, testInfo, "cron-detail");
  recordDiagnostics(diags);
  expect(diags.filter((d) => d.kind === "pageerror")).toEqual([]);
});

// Unknown routes and unknown ids are covered in `edge.spec.ts`.
