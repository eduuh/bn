// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import type { ReactNode } from "react";
import { Home } from "./Home";
import type { TaskRow } from "../api/types";

const TODAY = "2026-08-03";

const env = (command: string, data: unknown) => ({
  ok: true,
  json: async () => ({ ok: true, command, data }),
});

/** A store row with the fields the screen actually reads; the rest are the store's real nulls. */
function task(over: Partial<TaskRow> & { id: string; summary: string }): TaskRow {
  return {
    status: "open",
    priority: null,
    created: null,
    completion: null,
    due: null,
    snooze: null,
    branch: null,
    src: null,
    projects: [],
    contexts: [],
    repo: "bn",
    day: null,
    est: null,
    doc: null,
    smart: null,
    stage: null,
    uid: null,
    mod: 0,
    ...over,
  };
}

/** Triaged (day today-or-earlier **and** `smart:ok`), unscoped, and scheduled-but-not-SMART. */
const READY = task({ id: "t1", summary: "join the board to tasks", day: TODAY, smart: "ok" });
const UNSCOPED = task({ id: "t2", summary: "something about the cost page" });
const NOT_SMART = task({ id: "t3", summary: "rework the handoff", day: TODAY, smart: "fail" });
/** Past the SMART gate but never picked for today — the other half of the gate. */
const UNSCHEDULED = task({ id: "t4", summary: "prune the stage board", smart: "ok" });

const IN_FLIGHT = task({
  id: "t9",
  summary: "make the drawing drill into its tasks",
  day: TODAY,
  smart: "ok",
  status: "wip",
  branch: "today-redesign",
  stage: "execute",
});

const STAGES = {
  today: TODAY,
  in_flight: 1,
  orphans: 1,
  stages: [
    { id: "pickup", glyph: "◦", position: 0, count: 0, tasks: [], orphans: [] },
    {
      id: "execute",
      glyph: "⚙",
      position: 4,
      count: 1,
      tasks: [
        {
          ...IN_FLIGHT,
          agent: {
            session_id: "sess-1",
            target: "bn:today-redesign",
            harness: "claude",
            health: "active",
            idle_secs: 30,
          },
        },
      ],
      orphans: [{ repo: "bn", branch: "legacy-ui", ts: 1_000 }],
    },
  ],
};

const DONE = {
  today: TODAY,
  day: TODAY,
  status: "done",
  count: 2,
  done: 1,
  cancelled: 1,
  tasks: [
    { ...task({ id: "t7", summary: "shipped the parser" }), status: "done", reason: null },
    {
      ...task({ id: "t8", summary: "canvas rewrite" }),
      status: "cancelled",
      reason: "wrong-approach",
    },
  ],
};

/** Deliberately *not* severity-ordered on the wire — the screen has to sort it. */
const ATTN = {
  items: [
    { id: "a1", severity: "info", title: "an informational note", created: 30, repo: "bn" },
    { id: "a2", severity: "urgent", title: "refresh-all cron failed", created: 10, repo: "bn" },
    { id: "a3", severity: "warn", title: "PR waiting on review", created: 20, repo: "bn" },
  ],
};

const ISSUES = { repos: [], count: 0, stale_repos: 0, unreachable_repos: 0 };

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
      // `status=done` first: it is also a `/api/tasks` prefix.
      if (u.includes("/api/tasks?status=done")) return env("tasks", DONE) as unknown as Response;
      if (u.startsWith("/api/tasks"))
        return env("tasks", {
          today: TODAY,
          all: false,
          day: null,
          repo: null,
          count: 5,
          tasks: [READY, UNSCOPED, NOT_SMART, UNSCHEDULED, IN_FLIGHT],
        }) as unknown as Response;
      if (u.startsWith("/api/pipeline/stages"))
        return env("pipeline-stages", STAGES) as unknown as Response;
      if (u.startsWith("/api/attn")) return env("attn", ATTN) as unknown as Response;
      if (u.startsWith("/api/issues")) return env("issues", ISSUES) as unknown as Response;
      throw new Error(`unexpected fetch: ${u}`);
    }),
  );
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("Today — the task lifecycle (#1033)", () => {
  it("offers Start only on triaged work, and says why on everything else", async () => {
    wrap(<Home />);
    await waitFor(() => expect(screen.getByText("join the board to tasks")).toBeTruthy());

    const starts = screen.getAllByRole("button", { name: /start/i });
    const enabled = starts.filter((b) => !(b as HTMLButtonElement).disabled);
    const disabled = starts.filter((b) => (b as HTMLButtonElement).disabled);

    // Exactly one task is both scheduled for today and past the SMART gate. Both halves are
    // load-bearing: t4 is `smart:ok` but unscheduled, t3 is scheduled but failed the gate.
    expect(enabled).toHaveLength(1);
    // The rest are shown as blocked rather than hidden — an absent button would read as "never
    // startable" instead of "not yet, and here is the fix".
    expect(disabled).toHaveLength(3);
    expect(disabled.map((b) => b.getAttribute("title"))).toEqual(
      expect.arrayContaining([
        expect.stringContaining("SMART gate"),
        expect.stringContaining("not scheduled"),
      ]),
    );
  });

  it("separates untriaged work from work that is ready", async () => {
    wrap(<Home />);
    // Wait for real data, not just the card frame: the counts read "loading…" until it lands.
    await waitFor(() => expect(screen.getByText("join the board to tasks")).toBeTruthy());
    expect(screen.getByText(/1 triaged/)).toBeTruthy();
    expect(screen.getByText(/3 waiting · cannot be started/)).toBeTruthy();
  });

  it("drills a pipeline stage into the tasks actually in it", async () => {
    wrap(<Home />);
    await waitFor(() => expect(screen.getByText("make the drawing drill into its tasks")).toBeTruthy());

    fireEvent.click(screen.getByRole("button", { name: /execute: 1 in flight/i }));

    await waitFor(() => expect(screen.getByText("In flight · execute")).toBeTruthy());
    // The regression this whole change exists to prevent: a node claiming work over an empty list.
    expect(screen.queryByText("Nothing is in execute.")).toBeNull();
    expect(screen.getByText("make the drawing drill into its tasks")).toBeTruthy();
  });

  it("names an orphan worktree instead of dropping it", async () => {
    wrap(<Home />);
    await waitFor(() => expect(screen.getByText("make the drawing drill into its tasks")).toBeTruthy());
    fireEvent.click(screen.getByRole("button", { name: /execute: 1 in flight/i }));
    await waitFor(() => expect(screen.getByText("bn/legacy-ui")).toBeTruthy());
  });

  it("links an in-flight task to the agent working it", async () => {
    wrap(<Home />);
    await waitFor(() => expect(screen.getByText(/claude · active/)).toBeTruthy());
    const link = screen.getByTitle(/open in the fleet/);
    expect(link.getAttribute("href")).toBe("/agents/sess-1");
  });

  it("lists cancelled work without ever counting it as finished", async () => {
    wrap(<Home />);
    await waitFor(() => expect(screen.getByText("canvas rewrite")).toBeTruthy());
    // Both are listed…
    expect(screen.getByText("shipped the parser")).toBeTruthy();
    // …but the header keeps the two figures apart.
    expect(screen.getByText("1 finished · 1 cancelled")).toBeTruthy();
    expect(screen.getByText("wrong-approach")).toBeTruthy();
  });

  it("sorts Needs-you by severity so an urgent item cannot be buried", async () => {
    wrap(<Home />);
    await waitFor(() => expect(screen.getByText("refresh-all cron failed")).toBeTruthy());
    const titles = screen
      .getAllByText(/an informational note|refresh-all cron failed|PR waiting on review/)
      .map((n) => n.textContent);
    expect(titles).toEqual([
      "refresh-all cron failed",
      "PR waiting on review",
      "an informational note",
    ]);
  });
});
