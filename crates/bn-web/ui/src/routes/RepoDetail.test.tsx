// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import type { ReactNode } from "react";
import { RepoDetail } from "./RepoDetail";
import type {
  ActionItem,
  FleetView,
  LiveAgent,
  RepoIssuesView,
  RepoRollup,
} from "../api/types";
import * as writes from "../api/writes";

const rollup = (p: Partial<RepoRollup> = {}): RepoRollup => ({
  repo: "bn",
  managed: true,
  active_agents: 0,
  open_prs: 0,
  session_count: 0,
  last_activity_ts: 1_000,
  aic: 12.5,
  cost_usd: 3.25,
  pool: {
    enabled: false,
    total: 0,
    warm: 0,
    leased: 0,
    idle: 0,
    last_warmed_ts: null,
    slots: [],
  },
  links: null,
  sessions: [],
  ...p,
});

const agent = (p: Partial<LiveAgent> = {}): LiveAgent => ({
  session_id: "sess-1",
  harness: "copilot",
  repo: "bn",
  branch: "main",
  target: "bn:main",
  last_activity_ts: 1_000,
  idle_secs: 0,
  health: "active",
  role: "worker",
  turns: 0,
  tool_calls: 0,
  input_tokens: 0,
  output_tokens: 0,
  cache_read_tokens: 0,
  ...p,
});

const action = (p: Partial<ActionItem> = {}): ActionItem => ({
  id: "a1",
  type: "todo",
  title: "some task",
  source: "note",
  severity: "info",
  repo: null,
  branch: null,
  link: null,
  detail: null,
  payload: {},
  suggested_prompt: "do the thing",
  default_urgency: "queued",
  created: 0,
  stale: false,
  ...p,
});

interface Fixtures {
  repos?: RepoRollup[];
  fleet?: FleetView;
  issues?: RepoIssuesView;
  actions?: ActionItem[];
}

function stubFetch(f: Fixtures) {
  const fleet: FleetView = f.fleet ?? {
    agents: [],
    active: 0,
    idle: 0,
    stuck: 0,
    total_tokens: 0,
  };
  const issues: RepoIssuesView = f.issues ?? { repo: "bn", slug: "eduuh/bn", issues: [] };
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string | URL) => {
      const u = String(url);
      const env = (command: string, data: unknown) => ({
        ok: true,
        json: async () => ({ ok: true, command, data }),
      });
      if (u.includes("/issues")) return env("repo-issues", issues) as unknown as Response;
      if (u.startsWith("/api/repos")) return env("repos", { repos: f.repos ?? [rollup()] }) as unknown as Response;
      if (u.startsWith("/api/fleet")) return env("fleet", fleet) as unknown as Response;
      if (u.startsWith("/api/actions"))
        return env("actions", {
          count: (f.actions ?? []).length,
          actions: f.actions ?? [],
          cron_activity: [],
        }) as unknown as Response;
      throw new Error(`unexpected fetch: ${u}`);
    }),
  );
}

function wrap(node: ReactNode, repo = "bn") {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={qc}>
      <MemoryRouter initialEntries={[`/repos/${repo}`]}>
        <Routes>
          <Route path="/repos/:repo" element={node} />
        </Routes>
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("RepoDetail command center (#556)", () => {
  it("renders the orchestrator control, cost jump, and empty-issues sections", async () => {
    stubFetch({});
    wrap(<RepoDetail />);
    await waitFor(() => expect(screen.getByRole("heading", { name: "bn" })).toBeTruthy());
    // Ask #1: a Start-orchestrator button when none is running.
    expect(screen.getByRole("button", { name: /start orchestrator/i })).toBeTruthy();
    // Ask #2: the issues section renders (empty best-effort state).
    expect(await screen.findByText(/No open issues found/i)).toBeTruthy();
    // Ask #3: the cost jump.
    expect(screen.getByText(/API-equiv benchmark/i)).toBeTruthy();
    expect(screen.getByRole("link", { name: /View details/i })).toBeTruthy();
  });

  it("POSTs the orchestrator start when confirmed", async () => {
    const spy = vi.spyOn(writes, "startRepoOrchestrator").mockResolvedValue({});
    vi.stubGlobal("confirm", vi.fn(() => true));
    stubFetch({});
    wrap(<RepoDetail />);
    await waitFor(() => expect(screen.getByRole("heading", { name: "bn" })).toBeTruthy());
    fireEvent.click(screen.getByRole("button", { name: /start orchestrator/i }));
    await waitFor(() => expect(spy).toHaveBeenCalledWith("bn"));
  });

  it("shows a running orchestrator link instead of the start button", async () => {
    stubFetch({ fleet: { agents: [agent({ role: "orchestrator" })], active: 1, idle: 0, stuck: 0, total_tokens: 0 } });
    wrap(<RepoDetail />);
    await waitFor(() => expect(screen.getByText(/Orchestrator supervising this repo/i)).toBeTruthy());
    expect(screen.queryByRole("button", { name: /start orchestrator/i })).toBeNull();
  });

  it("lists open issues and opens the composer on Start", async () => {
    stubFetch({
      issues: {
        repo: "bn",
        slug: "eduuh/bn",
        issues: [{ number: 556, title: "per-repo command center", url: "https://github.com/eduuh/bn/issues/556", state: "open" }],
      },
    });
    wrap(<RepoDetail />);
    await waitFor(() => expect(screen.getByText(/per-repo command center/i)).toBeTruthy());
    fireEvent.click(screen.getByRole("button", { name: /^Start$/i }));
    // The reused StartAgentComposer opens with the issue's fix-and-PR prompt pre-filled.
    await waitFor(() => expect(screen.getByText(/Fixes #556/)).toBeTruthy());
  });

  it("shows each pool slot's holder, not just the counts", async () => {
    stubFetch({
      repos: [
        rollup({
          pool: {
            enabled: true,
            total: 3,
            warm: 1,
            leased: 1,
            idle: 2,
            last_warmed_ts: 900,
            slots: [
              { n: 1, leased: true, warm: true, branch: "feat/x", holder: "eduuh", age_secs: 60, warmed_ts: 900 },
              { n: 2, leased: false, warm: true, branch: null, holder: null, age_secs: null, warmed_ts: 900 },
              { n: 3, leased: false, warm: false, branch: null, holder: null, age_secs: null, warmed_ts: null },
            ],
          },
        }),
      ],
    });
    wrap(<RepoDetail />);
    await waitFor(() => expect(screen.getByText("Warm pool")).toBeTruthy());
    // The question counts can't answer: which branch is holding a slot, and who took it.
    expect(screen.getByText("feat/x")).toBeTruthy();
    expect(screen.getByText("eduuh")).toBeTruthy();
    // Each slot's own state — leased vs warm vs never built.
    expect(screen.getByText("leased")).toBeTruthy();
    expect(screen.getByText("cold")).toBeTruthy();
  });

  it("explains the un-pooled state instead of just showing nothing", async () => {
    stubFetch({});
    wrap(<RepoDetail />);
    await waitFor(() => expect(screen.getByText("Warm pool")).toBeTruthy());
    expect(screen.getByText(/Not pooled/)).toBeTruthy();
    // Points at the two ways to turn it on rather than leaving it a dead state.
    expect(screen.getByText(/bn wt pool set 2/)).toBeTruthy();
  });

  it("surfaces a cross-repo task that references this repo but is filed elsewhere", async () => {
    stubFetch({
      actions: [
        action({ id: "x1", repo: "forge", title: "wire forge to send events to bn" }),
        action({ id: "x2", repo: "forge", title: "unrelated forge cleanup" }),
      ],
    });
    wrap(<RepoDetail />);
    await waitFor(() => expect(screen.getByText(/Cross-repo tasks touching this repo/i)).toBeTruthy());
    expect(screen.getByText(/wire forge to send events to bn/i)).toBeTruthy();
    expect(screen.queryByText(/unrelated forge cleanup/i)).toBeNull();
  });
});
