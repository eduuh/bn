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

beforeEach(() => {
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
      if (u.startsWith("/api/today/agents"))
        return env("today-agents", TODAY_AGENTS) as unknown as Response;
      if (u.startsWith("/api/fleet")) return env("fleet", FLEET) as unknown as Response;
      throw new Error(`unexpected fetch: ${u}`);
    }),
  );
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

  it("fires the manual trigger via POST /api/today/run", async () => {
    const spy = vi.spyOn(writes, "runTodayAgent").mockResolvedValue({
      report: { sources: 1, items: 1, added: 0 },
      suggestions: [],
    });
    wrap(<Today />);
    await waitFor(() => expect(screen.getByText("Suggested tasks")).toBeTruthy());
    fireEvent.click(screen.getByRole("button", { name: /Run/ }));
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
