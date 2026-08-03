// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import type { ReactNode } from "react";
import { Today } from "./Today";
import * as writes from "../api/writes";

/** Envelope helper — reads unwrap `{ ok, data }`. */
const env = (command: string, data: unknown) => ({
  ok: true,
  json: async () => ({ ok: true, command, data }),
});

const GOALS = {
  daily: { date: "2026-07-16", done: 0, goals: [] },
  quarter: null,
};
const WRAP = { date: "2026-07-16", merged_prs: [], unnoted_commits: [], open_todos: [] };
const COST = { by_day: [] };
const FLEET = { active: 0, agents: [] };

/** A today-cron catalog row (`catalog_data` shape). */
const TODAY_CRONS = {
  count: 1,
  agents: [
    {
      job: "wt-cleanup",
      name: "wt-cleanup",
      kind: "cron",
      scope: "global",
      declared: true,
      on_demand: false,
      schedule: "*:0/15",
      description: "prune merged worktrees",
      has_script: true,
      network: false,
      catch_up: false,
      last_run: 1_000,
      last_exit: 0,
      ok: true,
      runs: 3,
      failures: 0,
    },
  ],
};

/** A today-agent catalog row (staff agent). */
const TODAY_AGENTS = {
  count: 1,
  agents: [
    {
      job: "staff-scout",
      name: "scout",
      kind: "staff-agent",
      scope: "global",
      declared: true,
      on_demand: false,
      schedule: "Mon..Fri *-*-* 08:30:00",
      description: "scout the backlog",
      has_script: false,
      network: true,
      catch_up: false,
      last_run: 2_000,
      last_exit: 0,
      ok: true,
      runs: 5,
      failures: 0,
      skill: "investigate",
    },
  ],
};

/** `GET /api/today/agent` — no hosted agent has ever been started on this box. */
const NO_TODAY_AGENT = { running: false, agent: null };

/** A live hosted agent: recorded, alive in the fleet, and steerable. */
const LIVE_TODAY_AGENT = {
  running: true,
  health: "active",
  control: "tmux",
  last_activity_ts: 3_000,
  output_tokens: 120,
  agent: {
    date: "2026-07-16",
    session_id: "sess-today-1",
    harness: "claude",
    repo: "today",
    branch: "2026-07-16",
    target: "today:2026-07-16",
    tmux_target: "planning:today",
    cwd: "/home/u/projects/personal-notes",
    status: "running",
    started_at: 2_500,
  },
};

const SUGGESTIONS = {
  count: 1,
  suggestions: [
    {
      id: "gh-prs-pr-1",
      title: "Review PR 1",
      rationale: "Surfaced by the 'gh-prs' cron on 2026-07-16",
      source: "gh-prs",
      source_key: "pr-1",
      url: "https://example.test/pr/1",
      created_at: 2_000,
      status: "suggested",
    },
  ],
};

function wrap(node: ReactNode) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={qc}>
      <MemoryRouter>{node}</MemoryRouter>
    </QueryClientProvider>,
  );
}

/** Installs the fetch stub. `hostedAgent` is what `GET /api/today/agent` answers, so a test can
 * pick between "never started" and "live" without restubbing every other route. */
function stubFetch(hostedAgent: unknown = NO_TODAY_AGENT) {
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string | URL) => {
      const u = String(url);
      if (u.startsWith("/api/suggestions"))
        return env("suggestions", SUGGESTIONS) as unknown as Response;
      if (u.startsWith("/api/goals")) return env("goals", GOALS) as unknown as Response;
      if (u.startsWith("/api/wrap")) return env("wrap", WRAP) as unknown as Response;
      if (u.startsWith("/api/cost")) return env("cost", COST) as unknown as Response;
      if (u.startsWith("/api/today/crons"))
        return env("today-crons", TODAY_CRONS) as unknown as Response;
      // Order matters: `/api/today/agents` (the roster) must be matched before `/api/today/agent`
      // (the one hosted session), or the plural route would never be reached.
      if (u.startsWith("/api/today/agents"))
        return env("today-agents", TODAY_AGENTS) as unknown as Response;
      if (u.startsWith("/api/today/agent"))
        return env("today-agent", hostedAgent) as unknown as Response;
      if (u.startsWith("/api/agents/")) return env("activity", { entries: [] }) as unknown as Response;
      if (u.startsWith("/api/fleet")) return env("fleet", FLEET) as unknown as Response;
      throw new Error(`unexpected fetch: ${u}`);
    }),
  );
}

beforeEach(() => {
  stubFetch();
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("Today — suggested tasks (#577)", () => {
  it("renders Today's agent suggestions with their source finding link", async () => {
    wrap(<Today />);
    await waitFor(() => expect(screen.getByText("Suggested tasks")).toBeTruthy());
    const link = await screen.findByRole("link", { name: /Review PR 1/ });
    expect(link.getAttribute("href")).toBe("https://example.test/pr/1");
    expect(screen.getByText(/Surfaced by the 'gh-prs' cron/)).toBeTruthy();
  });

  it("fires the offline re-synthesis via POST /api/today/run", async () => {
    const spy = vi.spyOn(writes, "runTodayAgent").mockResolvedValue({
      report: { sources: 1, items: 1, added: 0 },
      suggestions: [],
    });
    wrap(<Today />);
    await waitFor(() => expect(screen.getByText("Suggested tasks")).toBeTruthy());
    fireEvent.click(screen.getByRole("button", { name: /Re-synthesize/ }));
    await waitFor(() => expect(spy).toHaveBeenCalled());
  });

  it("reviews a suggestion via POST /api/suggestions/:id", async () => {
    const spy = vi
      .spyOn(writes, "setSuggestionStatus")
      .mockResolvedValue({ id: "gh-prs-pr-1", status: "accepted" });
    wrap(<Today />);
    await waitFor(() => expect(screen.getByText("Suggested tasks")).toBeTruthy());
    fireEvent.click(await screen.findByRole("button", { name: /Accept Review PR 1/ }));
    await waitFor(() => expect(spy).toHaveBeenCalledWith("gh-prs-pr-1", "accepted"));
  });
});

describe("Today — the hosted agent", () => {
  it("offers Start when no agent has ever run, and posts to /api/today/agent/start", async () => {
    const spy = vi.spyOn(writes, "startTodayAgent").mockResolvedValue({});
    wrap(<Today />);
    await waitFor(() => expect(screen.getByText("Today's agent")).toBeTruthy());
    expect(screen.getByText("not running")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: /Start agent/ }));
    await waitFor(() => expect(spy).toHaveBeenCalled());
  });

  it("streams and steers a live agent, keyed by its repo:branch target", async () => {
    vi.unstubAllGlobals();
    stubFetch(LIVE_TODAY_AGENT);
    wrap(<Today />);
    await waitFor(() => expect(screen.getByText("running")).toBeTruthy());
    // The steering identity is on the page, and the Steer composer is live rather than greyed —
    // `control: "tmux"` is what SteerPanel keys that decision off.
    expect(await screen.findByText("today:2026-07-16")).toBeTruthy();
    expect(screen.getByText("planning:today")).toBeTruthy();
    expect(screen.getByText("Steer")).toBeTruthy();
    const composer = screen.getByPlaceholderText(/Message the agent/);
    expect((composer as HTMLTextAreaElement).disabled).toBe(false);
  });

  it("shows a recorded-but-dead agent as stopped instead of offering a dead composer", async () => {
    vi.unstubAllGlobals();
    stubFetch({ ...LIVE_TODAY_AGENT, running: false, control: null, health: null });
    wrap(<Today />);
    // Wait for the record itself, not the badge: `not running` is also the pre-load default, so
    // asserting on it alone would pass before the response ever arrived.
    expect(await screen.findByText(/no live session/)).toBeTruthy();
    expect(screen.getByText("not running")).toBeTruthy();
    expect(screen.queryByText("Steer")).toBeNull();
  });
});

describe("Today — today's crons + agents (#575)", () => {
  it("renders today's scheduled crons and agents with their schedule", async () => {
    wrap(<Today />);
    await waitFor(() => expect(screen.getByText("Today's crons")).toBeTruthy());
    expect(screen.getByText("Today's agents")).toBeTruthy();
    // The cron row (by job name) and the staff agent row (by roster display name).
    expect(await screen.findByText("wt-cleanup")).toBeTruthy();
    expect(await screen.findByText("scout")).toBeTruthy();
    expect(screen.getByText("*:0/15")).toBeTruthy();
  });

  it("fires the shared run-now trigger via runCronJob (POST /api/cron/run)", async () => {
    const spy = vi.spyOn(writes, "runCronJob").mockResolvedValue({});
    vi.spyOn(window, "confirm").mockReturnValue(true);
    wrap(<Today />);
    await waitFor(() => expect(screen.getByText("Today's crons")).toBeTruthy());
    fireEvent.click(await screen.findByRole("button", { name: /^run$/ }));
    await waitFor(() => expect(spy).toHaveBeenCalledWith("wt-cleanup"));
  });
});
