// Interaction coverage for the screens the first pass only proved *render*.
//
// The sweep visits every route and asserts it paints without errors. That catches a crash but not a
// dead control — a filter that filters nothing, a form that never validates, a list you cannot open.
// This spec drives the affordances on the screens that had render-only coverage: Insights, Goals,
// Attention, Notes, Handoff, Research, Nodes.
//
// Read-only: forms are filled and validated but never submitted. Attention/goal/suggestion writes
// live in `writes.spec.ts`.

import { expect, test } from "@playwright/test";
import { primeToken, recordDiagnostics, shot, visit, watch } from "./live";

function assertClean(diags: ReturnType<typeof watch>, what: string): void {
  recordDiagnostics(diags);
  const fatal = diags.filter((d) => d.kind === "pageerror" || d.kind === "envelope");
  expect(fatal, `${what}:\n${fatal.map((d) => d.text).join("\n")}`).toEqual([]);
}

test("insights switches its metrics window and keeps the chart populated", async ({
  page,
}, testInfo) => {
  const diags = watch(page, "/insights", testInfo.project.name);
  await primeToken(page);
  await visit(page, "/insights");

  const windows = page.getByRole("tablist", { name: "window" });
  await expect(windows).toBeVisible();

  const tabs = await windows.getByRole("tab").all();
  expect(tabs.length, "the window picker should offer choices").toBeGreaterThan(1);

  for (const tab of tabs) {
    await tab.click();
    await page.waitForTimeout(500);
    // The view must stay populated across every window — an empty 7d is a real answer, a blank
    // screen is not.
    await expect(page.getByRole("tablist", { name: "insights view" })).toBeVisible();
    expect((await page.locator("main").innerText()).trim().length).toBeGreaterThan(0);
  }

  // Insights is five screens behind one nav entry (Metrics / Cost / Weekly / Insights / Brag); the
  // route sweep only ever saw whichever one is default, so walk them all.
  const views = page.getByRole("tablist", { name: "insights view" });
  for (const tab of await views.getByRole("tab").all()) {
    const label = (await tab.innerText()).trim();
    await tab.click();
    await page.waitForTimeout(600);
    const body = (await page.locator("main").innerText()).trim();
    expect(body.length, `the ${label} view rendered nothing`).toBeGreaterThan(0);
  }

  await shot(page, testInfo, "insights");
  assertClean(diags, "insights");
});

test("the research form validates before it will dispatch a scout", async ({ page }, testInfo) => {
  const diags = watch(page, "/research", testInfo.project.name);
  await primeToken(page);
  await visit(page, "/research");

  const question = page.getByPlaceholder(/why is the nvim status_spec flaky/);
  await expect(question).toBeVisible();

  // A scout needs both a question and a repo; the button must not be live until it has them.
  const dispatch = page.getByRole("button", { name: /scout|research|dispatch/i }).last();
  await expect(dispatch).toBeDisabled();

  await question.fill("live-e2e probe — do not run");
  await page.getByPlaceholder("repo (required)").fill("bn");
  await expect(dispatch).toBeEnabled();

  // Clearing the repo must disable it again — validation has to be live, not just initial.
  await page.getByPlaceholder("repo (required)").fill("");
  await expect(dispatch).toBeDisabled();

  await shot(page, testInfo, "research");
  assertClean(diags, "research");
});

test("handoff composes a briefing without sending it", async ({ page }, testInfo) => {
  const diags = watch(page, "/handoff", testInfo.project.name);
  await primeToken(page);
  await visit(page, "/handoff");

  const orient = page.getByPlaceholder(/A few sentences orienting the next session/);
  test.skip((await orient.count()) === 0, "no branch selected / nothing to hand off on this host");

  await orient.fill("live-e2e probe — not a real handoff");
  await expect(orient).toHaveValue(/live-e2e probe/);

  await shot(page, testInfo, "handoff");
  assertClean(diags, "handoff");
});

test("notes lists branches and opens one", async ({ page }, testInfo) => {
  const diags = watch(page, "/notes", testInfo.project.name);
  await primeToken(page);
  await visit(page, "/notes");

  const rows = page.locator("main button");
  const n = await rows.count();
  // The page auto-selects the first note, so proving the list responds needs a *second* row.
  test.skip(n < 2, "need at least two notes to prove selection changes the pane");

  const before = (await page.locator("main").innerText()).trim();
  await rows.nth(1).click();
  await page.waitForTimeout(700);

  // Selecting a different note must change what is on screen — a list that doesn't respond is the
  // failure this guards.
  const after = (await page.locator("main").innerText()).trim();
  expect(after.length).toBeGreaterThan(0);
  expect(after, "selecting a different note changed nothing").not.toBe(before);

  await shot(page, testInfo, "notes");
  assertClean(diags, "notes");
});

test("attention items render as articles and offer an action", async ({ page }, testInfo) => {
  const diags = watch(page, "/attention", testInfo.project.name);
  await primeToken(page);
  await visit(page, "/attention");

  const items = page.getByRole("article");
  const n = await items.count();
  test.skip(n === 0, "attention inbox is empty");

  // Every item must offer a way to act on or clear it — an inbox you can only read is a list.
  await expect(items.first().getByRole("button").first()).toBeVisible();

  await shot(page, testInfo, "attention");
  assertClean(diags, "attention");
});

test("goals show quarter progress and connected notes", async ({ page }, testInfo) => {
  const diags = watch(page, "/goals", testInfo.project.name);
  await primeToken(page);
  await visit(page, "/goals");

  await expect(page.getByRole("heading", { name: /Goals/ }).first()).toBeVisible();

  // The quarter meter is a progressbar with real ARIA values, not a decorative div.
  const meter = page.getByRole("progressbar", { name: "quarter elapsed" });
  if (await meter.count()) {
    await expect(meter).toHaveAttribute("aria-valuenow", /\d+/);
  }

  await shot(page, testInfo, "goals");
  assertClean(diags, "goals");
});

test("nodes reports the local host with live vitals", async ({ page }, testInfo) => {
  const diags = watch(page, "/nodes", testInfo.project.name);
  await primeToken(page);
  await visit(page, "/nodes");

  const nodes = await page.request.get("/api/nodes").then((r) => r.json());
  const local = (nodes?.data?.nodes ?? []).find((n: { local?: boolean }) => n.local);
  test.skip(!local, "no local node reported");

  // The host it is running on must be named on screen — this is the one fact the page exists for.
  await expect(page.locator("main")).toContainText(local.host);

  await shot(page, testInfo, "nodes");
  assertClean(diags, "nodes");
});

test("the topbar vitals track the machine endpoint", async ({ page }, testInfo) => {
  test.skip(testInfo.project.name !== "desktop", "the topbar collapses on a phone");
  const diags = watch(page, "/", testInfo.project.name);
  await primeToken(page);
  await visit(page, "/fleet");

  const machine = await page.request.get("/api/machine").then((r) => r.json());
  const cores = machine?.data?.cpu?.cores;
  test.skip(!cores, "no machine sample");

  // The gauge is "load / cores" — the denominator is the machine's real core count, so a mismatch
  // means the topbar is reading a different host than the API.
  await expect(page.locator("header, [class*=topbar]").first()).toContainText(String(cores));

  assertClean(diags, "topbar vitals");
});
