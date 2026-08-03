import { expect, test, type Page } from "@playwright/test";
import { seed } from "./fixtures";
import { primeToken, shot } from "./helpers";

const SESSION = "sess-x";
const TARGET = "bn:dispatch-web-redesign-500";

/** One row of the mocked activity feed. */
type Entry = {
  kind: "user" | "agent" | "tools";
  ts?: number;
  text?: string;
  summary?: string;
  tools?: { name: string; target: string }[];
};

/** Mock `GET /api/agents/:id/activity` — the session timeline the detail page reads. Serving it
 * deterministically means the test needs no real harness transcript on the runner. */
async function mockActivity(page: Page, entries: Entry[], total?: number): Promise<void> {
  const filled = entries.map((e) => ({
    ts: Math.floor(Date.now() / 1000) - 60,
    text: "",
    summary: "",
    tools: [],
    ...e,
  }));
  await page.route(`**/api/agents/${SESSION}/activity*`, (route) =>
    route.fulfill({
      json: {
        ok: true,
        command: "agent_activity",
        data: {
          session_id: SESSION,
          harness: "claude",
          entries: filled,
          total: total ?? filled.length,
          count: filled.length,
        },
        error: null,
      },
    }),
  );
}

// The agent-detail page reads `/api/agents/:id` (fleet detail) and `/api/agents/:id/activity` (the
// session timeline) — both need a live session, so we inject deterministic fakes (the issue
// explicitly allows an injectable fake fleet backend). Every *write* below (send / relaunch / kill
// / land) still hits the REAL server + stub `bn`, so we prove the UI issues the right authorized
// command.
test.beforeEach(async ({ page }) => {
  seed();
  await page.route(`**/api/agents/${SESSION}`, (route) =>
    route.fulfill({
      json: {
        ok: true,
        command: "agent",
        data: {
          agent: {
            session_id: SESSION,
            harness: "claude",
            repo: "bn",
            branch: "dispatch/web-redesign-500",
            target: TARGET,
            last_activity_ts: Math.floor(Date.now() / 1000) - 20,
            idle_secs: 20,
            health: "active",
            turns: 7,
            tool_calls: 12,
            input_tokens: 100000,
            output_tokens: 52000,
            cache_read_tokens: 96000,
          },
          latest_session: null,
          last_request: "build the mobile redesign (#479)",
          note: {
            repo: "bn",
            branch: "dispatch-web-redesign-500",
            front: {
              repo: "bn",
              branch: "dispatch-web-redesign-500",
              created: "2026-07-13",
              status: "active",
              pr: "https://github.com/eduuh/bn/pull/500",
              tags: [],
              collaborators: [],
              todos: [
                { id: "t1", text: "wire the rendered note into the agent view", created: "2026-07-13" },
                { id: "t2", text: "this todo is already closed", created: "2026-07-13", done: true },
              ],
              blockers: [{ id: "b1", text: "waiting on design review", created: "2026-07-13" }],
              decisions: [
                {
                  id: "d1",
                  text: "render markdown with react-markdown — see https://github.com/remarkjs/react-markdown",
                  created: "2026-07-13",
                },
              ],
              research: [],
              collab: [],
              ask: [],
              done_when: [{ id: "w1", text: "renders on mobile + desktop", created: "2026-07-13" }],
            },
            body:
              "# bn / dispatch-web-redesign-500\n\n## Goal\n\nMobile-first redesign of the dashboard\n\n" +
              "## Progress\n- 2026-07-13 — verify test: PASS\n\nDocs at https://docs.example.com/redesign\n\n" +
              "```sh\nnpm run build\nnpm run test\n```\n",
            goal: "Mobile-first redesign of the dashboard",
          },
          harnesses: [
            { name: "copilot", available: true },
            { name: "claude", available: true },
          ],
        },
        error: null,
      },
    }),
  );
});

test("agent detail: activity + steer/relaunch/kill/land issue real authorized writes", async ({ page }, testInfo) => {
  await mockActivity(page, [
    { kind: "user", text: "build the mobile redesign" },
    { kind: "agent", text: "Building the redesign — **42 tests pass**." },
  ]);
  await primeToken(page);
  await page.goto(`/agents/${SESSION}`);

  await expect(page.getByText("dispatch/web-redesign-500", { exact: true })).toBeVisible();
  // The activity feed reads the session transcript and renders it as prose, so the agent's message
  // is plain selectable text — not a fixed-width grid that wraps a 200-column pane into noise.
  await expect(page.getByText(/42 tests pass/)).toBeVisible();

  await shot(page, testInfo, "agent");

  // Steer: send a message → POST /send on the real server.
  await page
    .getByPlaceholder("Message the agent (⌘/Ctrl+Enter to send)…")
    .fill("focus on the mobile layout");
  const [sendReq] = await Promise.all([
    page.waitForRequest((r) => r.url().includes("/send") && r.method() === "POST"),
    page.getByRole("button", { name: "Send", exact: true }).click(),
  ]);
  expect(JSON.parse(sendReq.postData() ?? "{}").message).toContain("mobile layout");
  await expect(page.getByText("message sent")).toBeVisible();

  // Relaunch (soft).
  const [relaunchReq] = await Promise.all([
    page.waitForRequest((r) => r.url().includes("/relaunch") && r.method() === "POST"),
    page.getByRole("button", { name: "Relaunch", exact: true }).click(),
  ]);
  expect(relaunchReq.method()).toBe("POST");

  // Kill (confirm dialog).
  page.once("dialog", (d) => d.accept());
  const [killReq] = await Promise.all([
    page.waitForRequest((r) => r.url().includes("/kill") && r.method() === "POST"),
    page.getByRole("button", { name: "Kill" }).click(),
  ]);
  expect(killReq.method()).toBe("POST");

  // Landing is not on this page: `bn wt merge --all` is a repo-wide sweep, not this agent's PR, so
  // a button for it here misnames what it does. It lives on the repo/console surfaces.
  await expect(page.getByRole("button", { name: /Land ready PRs/ })).toHaveCount(0);
});

test("agent detail: switching harness relaunches in place under the chosen harness (#501)", async ({
  page,
}) => {
  await mockActivity(page, [{ kind: "agent", text: "idle — waiting for input" }]);
  await primeToken(page);
  await page.goto(`/agents/${SESSION}`);

  // The harness rides in the header's identity row (repo · branch · state · harness) as a chip, not
  // as a sidebar card of segmented buttons — switching stops and relaunches the agent, so it earns
  // one tap of disclosure rather than permanent real estate.
  const header = page.getByRole("heading", { level: 2 }).locator("xpath=..");
  const chip = header.getByRole("button", { name: /claude/ });
  await expect(chip).toBeVisible();
  await expect(page.getByRole("menu", { name: "Switch harness" })).toHaveCount(0);

  // Open it: the running harness is checked, the alternative is offered.
  await chip.click();
  const menu = page.getByRole("menu", { name: "Switch harness" });
  await expect(menu.getByRole("menuitem", { name: /claude/ })).toHaveAttribute(
    "aria-checked",
    "true",
  );

  // Switch to copilot: confirm → POST /relaunch {harness:"copilot"} on the agent's own target.
  page.once("dialog", (d) => d.accept());
  const [switchReq] = await Promise.all([
    page.waitForRequest((r) => r.url().includes("/relaunch") && r.method() === "POST"),
    menu.getByRole("menuitem", { name: /copilot/ }).click(),
  ]);
  expect(switchReq.url()).toContain(encodeURIComponent(TARGET));
  expect(JSON.parse(switchReq.postData() ?? "{}").harness).toBe("copilot");
  await expect(page.getByText(/restarting .* under copilot/)).toBeVisible();
});

test("agent detail: session note renders (PR, references, scripts, sections) — #484", async ({
  page,
}, testInfo) => {
  await mockActivity(page, [{ kind: "agent", text: "idle — waiting for input" }]);
  await primeToken(page);
  await page.goto(`/agents/${SESSION}`);

  const note = page.getByRole("region", { name: "Session note" });
  await expect(note.getByRole("heading", { name: "Session note" })).toBeVisible();

  // (1) Rendered markdown: the `## Goal` heading + its prose, not raw text.
  await expect(note.getByRole("heading", { name: "Goal", level: 2 })).toBeVisible();
  await expect(note.getByText("Mobile-first redesign of the dashboard")).toBeVisible();

  // (2) A prominent, clickable PR button pointing at the recorded PR url.
  const pr = note.getByRole("link", { name: /PR #500/ });
  await expect(pr).toBeVisible();
  await expect(pr).toHaveAttribute("href", "https://github.com/eduuh/bn/pull/500");

  // (3) Resource references: a note URL, linkified in the References list.
  const ref = note
    .getByRole("list", { name: "resource references" })
    .getByRole("link", { name: "docs.example.com/redesign" });
  await expect(ref).toHaveAttribute("href", "https://docs.example.com/redesign");

  // (4) Session scripts: the dispatch prompt + the captured command block.
  const scripts = note.locator("details", { hasText: "Session scripts" });
  await expect(scripts.getByText("build the mobile redesign (#479)")).toBeVisible();
  await expect(scripts.getByText("npm run build")).toBeVisible();

  // (5) Structured sections: blockers, done-when, and OPEN todos only (closed ones are dropped).
  await expect(note.getByRole("heading", { name: "Blockers" })).toBeVisible();
  await expect(note.getByText("waiting on design review")).toBeVisible();
  await expect(note.getByText("wire the rendered note into the agent view")).toBeVisible();
  await expect(note.getByText("this todo is already closed")).toHaveCount(0);

  // A full-page review screenshot (this view is tall) committed for the owner's design review.
  await page.waitForTimeout(300);
  await page.screenshot({ path: `e2e/screenshots/session-${testInfo.project.name}.png`, fullPage: true });
});

test("agent detail: the activity feed reflows prose and collapses tool runs", async ({
  page,
}, testInfo) => {
  // The failure this view exists to fix: the agent's real tmux pane is ~200 columns, so mirroring
  // it into a browser-width grid wrapped every line and doubled the TUI's box rules. Prose has no
  // fixed width, so a long paragraph must simply reflow â and the tool traffic that dominates a
  // long session must stay folded until asked for.
  const longProse =
    "Cockpit removed: `cmd/cockpit.rs`, the `bn orchestrate cockpit` subcommand tree, the " +
    "ensure-on-run hooks, the cockpit-ensure native job, and the prefix-P tmux binding. That " +
    "eliminates the planning:cockpit windows entirely, and the dashboard label with them.";
  await mockActivity(
    page,
    [
      { kind: "user", text: "remove the tmux cron dispatch and the cockpit" },
      {
        kind: "tools",
        summary: "14 tool calls · Read ×6, Edit ×5, Bash ×3",
        tools: [
          { name: "Read", target: "crates/bn-cli/src/cmd/cockpit.rs" },
          { name: "Bash", target: "Run the test suite" },
        ],
      },
      { kind: "agent", text: longProse },
    ],
    312,
  );

  await primeToken(page);
  await page.goto(`/agents/${SESSION}`);

  // Prose renders as markdown and wraps to the viewport — inline code is a real <code> element.
  await expect(page.getByText(/eliminates the planning:cockpit windows/)).toBeVisible();
  await expect(page.locator("code", { hasText: "cmd/cockpit.rs" }).first()).toBeVisible();

  // The page never scrolls sideways — the whole point. (The feed scrolls *vertically*; a
  // 200-column mirror is what used to force horizontal overflow.)
  const overflows = await page.evaluate(
    () => document.documentElement.scrollWidth > document.documentElement.clientWidth + 1,
  );
  expect(overflows).toBe(false);

  // A long session says how much of itself is on screen, rather than silently truncating.
  await expect(page.getByText(/last 3 of 312/)).toBeVisible();

  // Tool runs are folded to one line, and expand on demand.
  const toolRun = page.getByRole("button", { name: /14 tool calls/ });
  await expect(toolRun).toBeVisible();
  await expect(page.getByText("crates/bn-cli/src/cmd/cockpit.rs")).toHaveCount(0);
  await toolRun.click();
  await expect(page.getByText("crates/bn-cli/src/cmd/cockpit.rs")).toBeVisible();
  await expect(page.getByText("Run the test suite")).toBeVisible();

  await shot(page, testInfo, "agent-activity");
});
