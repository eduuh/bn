import { expect, test, type Page } from "@playwright/test";
import { seed } from "./fixtures";
import { primeToken, shot } from "./helpers";

const SESSION = "sess-x";
const TARGET = "bn:dispatch-web-redesign-500";

/** Mock the live-terminal WebSocket (`GET /ws/agents/:target/terminal`, #491): stream one pane
 * frame to the page so the xterm view renders deterministically — no real tmux/agent behind the
 * server. Registered before `goto`, since the component opens the socket on mount. */
async function mockTerminal(page: Page, lines: string[]): Promise<void> {
  await page.routeWebSocket(/\/ws\/agents\/.*\/terminal/, (ws) => {
    ws.send(JSON.stringify({ type: "frame", lines }));
  });
}

// The agent-detail page reads `/api/agents/:id` (fleet detail) and streams the live pane over a
// WebSocket (`…/terminal`, #491) — both need a live session, so we inject deterministic fakes (the
// issue explicitly allows an injectable fake fleet backend). Every *write* below (send / relaunch /
// kill / land) still hits the REAL server + stub `bn`, so we prove the UI issues the right
// authorized command.
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

test("agent detail: pane + steer/relaunch/kill/land issue real authorized writes", async ({ page }, testInfo) => {
  await mockTerminal(page, ["● building the redesign…", "✓ 42 tests pass"]);
  await primeToken(page);
  await page.goto(`/agents/${SESSION}`);

  await expect(page.getByText("dispatch/web-redesign-500", { exact: true })).toBeVisible();
  // The live terminal (#491) streams over a WebSocket and renders inline — the mocked frame lands
  // in the xterm rows (no expand needed; the old collapsed <pre> pane is gone).
  const terminal = page.getByRole("img", { name: `Live terminal for ${TARGET}` });
  await expect(terminal.locator(".xterm-rows")).toContainText(/42 tests pass/);

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

  // Land ready PRs (confirm dialog) → POST /api/land {repo}.
  page.once("dialog", (d) => d.accept());
  const [landReq] = await Promise.all([
    page.waitForRequest((r) => r.url().includes("/api/land") && r.method() === "POST"),
    page.getByRole("button", { name: /Land ready PRs/ }).click(),
  ]);
  expect(JSON.parse(landReq.postData() ?? "{}").repo).toBe("bn");
});

test("agent detail: switching harness relaunches in place under the chosen harness (#501)", async ({
  page,
}) => {
  await mockTerminal(page, ["idle — waiting for input"]);
  await primeToken(page);
  await page.goto(`/agents/${SESSION}`);

  const panel = page.getByRole("group", { name: "Harness: switch harness" });
  await expect(panel.getByText("Harness", { exact: true })).toBeVisible();
  // The agent runs under claude → that option is the active one.
  await expect(panel.getByRole("button", { name: /claude/ })).toHaveAttribute("aria-pressed", "true");

  // Switch to copilot: confirm → POST /relaunch {harness:"copilot"} on the agent's own target.
  page.once("dialog", (d) => d.accept());
  const [switchReq] = await Promise.all([
    page.waitForRequest((r) => r.url().includes("/relaunch") && r.method() === "POST"),
    panel.getByRole("button", { name: /copilot/ }).click(),
  ]);
  expect(switchReq.url()).toContain(encodeURIComponent(TARGET));
  expect(JSON.parse(switchReq.postData() ?? "{}").harness).toBe("copilot");
  await expect(page.getByText(/restarting .* under copilot/)).toBeVisible();
});

test("agent detail: session note renders (PR, references, scripts, sections) — #484", async ({
  page,
}, testInfo) => {
  await mockTerminal(page, ["idle — waiting for input"]);
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

test("agent detail: live terminal renders an ANSI/box-drawing frame — no raw escapes (#491/#496)", async ({
  page,
}, testInfo) => {
  // A capture frame with real terminal control bytes: clear-screen + cursor-home, SGR colours, a
  // spinner glyph, and box-drawing — the exact "garbled" input a raw <pre> would dump verbatim.
  const ansiFrame = [
    "\u001b[2J\u001b[H\u001b[36m╭─ bn ─────────────────╮\u001b[0m",
    "\u001b[36m│\u001b[0m \u001b[32m● building the redesign…\u001b[0m",
    "\u001b[36m│\u001b[0m \u001b[1;33m⠋\u001b[0m running \u001b[31mcargo test\u001b[0m",
    "\u001b[36m╰──────────────────────╯\u001b[0m",
    "\u001b[32m✓ 42 tests pass\u001b[0m",
  ];
  // The streamed terminal (#491) carries the ANSI payload over the WebSocket; xterm interprets it.
  await mockTerminal(page, ansiFrame);

  await primeToken(page);
  await page.goto(`/agents/${SESSION}`);

  const terminal = page.getByRole("img", { name: `Live terminal for ${TARGET}` });
  const rows = terminal.locator(".xterm-rows");
  // The escapes are interpreted: the visible text + box-drawing land as real glyphs…
  await expect(rows).toContainText("building the redesign");
  await expect(rows).toContainText("cargo test");
  await expect(rows).toContainText("42 tests pass");
  await expect(rows).toContainText("│");
  await expect(rows).toContainText("╭");

  // …and NOTHING raw leaks through: no ESC byte, no CSI/SGR remnants anywhere in the pane.
  const rendered = await terminal.innerText();
  expect(rendered).not.toMatch(/\u001b/);
  expect(rendered).not.toContain("[2J");
  expect(rendered).not.toContain("[36m");
  expect(rendered).not.toContain("[0m");

  await shot(page, testInfo, "agent-pane-ansi");
});
