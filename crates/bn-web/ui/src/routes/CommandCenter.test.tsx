// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import type { ReactNode } from "react";
import { CommandCenter } from "./CommandCenter";
import type { ActionItem } from "../api/types";
import * as writes from "../api/writes";

const env = (command: string, data: unknown) => ({
  ok: true,
  json: async () => ({ ok: true, command, data }),
});

/** A todo action fixture — `stale` drives the fold + de-emphasis. */
function todo(n: number, opts: { stale?: boolean } = {}): ActionItem {
  return {
    id: `todo:bn/feat/t${n}`,
    type: "todo",
    title: `todo number ${n}`,
    source: "note · bn/feat",
    severity: "info",
    repo: "bn",
    branch: "feat",
    link: null,
    detail: null,
    payload: { repo: "bn", branch: "feat", target: "bn:feat" },
    suggested_prompt: `pick up todo ${n}`,
    default_urgency: "queued",
    created: 1_000 - n, // lower n = fresher (ranked first)
    stale: opts.stale ?? false,
  };
}

const BLOCKER: ActionItem = {
  id: "blocker:bn/feat/b1",
  type: "blocker",
  title: "blocked on the api",
  source: "note · bn/feat",
  severity: "urgent",
  repo: "bn",
  branch: "feat",
  link: null,
  detail: null,
  payload: { repo: "bn", branch: "feat", target: "bn:feat" },
  suggested_prompt: "unblock it",
  default_urgency: "urgent",
  created: 5_000,
  stale: false,
};

// One urgent blocker pinned on top, then 9 todos (2 flagged stale) — more than the cap of 6.
const ACTIONS = {
  count: 10,
  actions: [
    BLOCKER,
    todo(1),
    todo(2),
    todo(3),
    todo(4),
    todo(5),
    todo(6),
    todo(7),
    todo(8, { stale: true }),
    todo(9, { stale: true }),
  ],
  // #557 — the observational cron-activity feed rides alongside the action list.
  cron_activity: [
    {
      job: "wt-cleanup",
      scope: "global",
      ok: false,
      last_exit: 1,
      last_run: 1_000,
      runs: 4,
      failures: 1,
      declared: true,
      schedule: "*:0/15",
      description: null,
      last_output: "boom: it broke",
      severity: "urgent",
    },
  ],
};

// #557 — the triggerable-agent catalog: one staff agent + one plain cron job.
const CATALOG = {
  count: 2,
  agents: [
    {
      job: "staff-mentor",
      name: "mentor",
      kind: "staff-agent",
      scope: "global",
      declared: true,
      on_demand: false,
      schedule: "*-*-* 06:30:00",
      description: "the mentor agent",
      has_script: false,
      network: true,
      catch_up: false,
      last_run: 2_000,
      last_exit: 0,
      ok: true,
      runs: 3,
      failures: 0,
      skill: "staff/mentor",
      model: "opus",
      harness: "claude",
    },
    {
      job: "wt-cleanup",
      name: "wt-cleanup",
      kind: "cron",
      scope: "global",
      declared: true,
      on_demand: false,
      schedule: "*:0/15",
      description: null,
      has_script: true,
      network: false,
      catch_up: false,
      last_run: 1_000,
      last_exit: 1,
      ok: false,
      runs: 4,
      failures: 1,
    },
  ],
};

// One completed todo carrying both a local and a remote note jump (#561).
const COMPLETED = {
  count: 1,
  completed: [
    {
      id: "done-todo:bn/feat/t1",
      type: "todo",
      title: "shipped the thing",
      source: "note · bn/feat",
      repo: "bn",
      branch: "feat",
      link: null,
      detail: null,
      completed: 1_700_000_000,
      note: {
        local_path: "/notes/testhost/bn/feat/note.md",
        remote_url: "https://github.com/eduuh/branch-notes/blob/main/testhost/bn/feat/note.md",
      },
    },
  ],
};

function wrap(node: ReactNode, initialEntries: string[] = ["/"]) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={qc}>
      <MemoryRouter initialEntries={initialEntries}>{node}</MemoryRouter>
    </QueryClientProvider>,
  );
}

beforeEach(() => {
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string | URL) => {
      const u = String(url);
      if (u.startsWith("/api/agents/catalog"))
        return env("agents-catalog", CATALOG) as unknown as Response;
      if (u.startsWith("/api/actions")) return env("actions", ACTIONS) as unknown as Response;
      if (u.startsWith("/api/fleet")) return env("fleet", { count: 0, agents: [] }) as unknown as Response;
      if (u.startsWith("/api/orchestrate"))
        return env("orchestrate", { count: 0, runs: [] }) as unknown as Response;
      if (u.startsWith("/api/reviews")) return env("reviews", { reviews: [] }) as unknown as Response;
      if (u.startsWith("/api/completed")) return env("completed", COMPLETED) as unknown as Response;
      throw new Error(`unexpected fetch: ${u}`);
    }),
  );
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("Command center todo grouping (#546)", () => {
  it("pins the urgent blocker above the capped todo section", async () => {
    wrap(<CommandCenter />);
    await waitFor(() => expect(screen.getByText("blocked on the api")).toBeTruthy());
    // Only the first TODO_CAP (6) todos render before the expander.
    expect(screen.getByText("todo number 1")).toBeTruthy();
    expect(screen.getByText("todo number 6")).toBeTruthy();
    expect(screen.queryByText("todo number 7")).toBeNull();
    // A dedicated Todos section header with the full count.
    const head = screen.getByRole("heading", { name: "Todos" });
    expect(head).toBeTruthy();
  });

  it("reveals the full backlog behind the show-all expander", async () => {
    wrap(<CommandCenter />);
    await waitFor(() => expect(screen.getByText("todo number 1")).toBeTruthy());
    expect(screen.queryByText("todo number 9")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: /show all \(9\)/ }));
    await waitFor(() => expect(screen.getByText("todo number 9")).toBeTruthy());
    // Collapse again.
    fireEvent.click(screen.getByRole("button", { name: /show fewer/ }));
    await waitFor(() => expect(screen.queryByText("todo number 9")).toBeNull());
  });

  it("keeps one-click Start agent on every row, including expanded stale ones", async () => {
    wrap(<CommandCenter />);
    await waitFor(() => expect(screen.getByText("blocked on the api")).toBeTruthy());
    fireEvent.click(screen.getByRole("button", { name: /show all \(9\)/ }));
    await waitFor(() => expect(screen.getByText("todo number 9")).toBeTruthy());
    // 1 blocker + 9 todos = 10 Start-agent CTAs once everything is expanded.
    expect(screen.getAllByRole("button", { name: /Start agent/ })).toHaveLength(10);
    // The stale todos carry a "stale" marker.
    expect(screen.getAllByText("stale")).toHaveLength(2);
  });
});

describe("Command center reviews tab (#558)", () => {
  it("folds the plan-review inbox into a Reviews tab (graceful empty state)", async () => {
    wrap(<CommandCenter />);
    await waitFor(() => expect(screen.getByText("blocked on the api")).toBeTruthy());
    // Actions tab is the default — the action list shows, the reviews inbox does not.
    expect(screen.queryByText("No plan reviews")).toBeNull();
    const reviewsTab = screen.getByRole("tab", { name: /Reviews/ });
    // Radix Tabs use automatic activation (on focus); jsdom's click doesn't focus, so do both.
    fireEvent.focus(reviewsTab);
    fireEvent.click(reviewsTab);
    await waitFor(() => expect(screen.getByText("No plan reviews")).toBeTruthy());
  });

  it("deep-links to the Reviews tab via ?tab=reviews", async () => {
    wrap(<CommandCenter />, ["/?tab=reviews"]);
    await waitFor(() => expect(screen.getByText("No plan reviews")).toBeTruthy());
  });
});

describe("Command center automation hub (#557)", () => {
  it("renders the cron-activity feed and the triggerable catalog", async () => {
    wrap(<CommandCenter />);
    await waitFor(() => expect(screen.getByRole("heading", { name: "Automation" })).toBeTruthy());
    // The staff agent shows by roster name with an "agent" badge; the cron job by its job name.
    await waitFor(() => expect(screen.getByText("mentor")).toBeTruthy());
    // Both the activity feed and the catalog surface the failing wt-cleanup job.
    expect(screen.getAllByText("wt-cleanup").length).toBeGreaterThan(0);
    // A re-trigger control per cataloged entry (2 in the fixture).
    expect(screen.getAllByRole("button", { name: /re-run/ })).toHaveLength(2);
  });

  it("re-triggers a cataloged job via POST /api/cron/run", async () => {
    const spy = vi.spyOn(writes, "runCronJob").mockResolvedValue(undefined as never);
    vi.stubGlobal("confirm", vi.fn(() => true));
    wrap(<CommandCenter />);
    await waitFor(() => expect(screen.getByText("mentor")).toBeTruthy());
    fireEvent.click(screen.getAllByRole("button", { name: /re-run/ })[0]);
    await waitFor(() => expect(spy).toHaveBeenCalledWith("staff-mentor"));
  });
});

describe("Command center completed tab (#561)", () => {
  it("lists completed tasks with local + remote note jumps", async () => {
    wrap(<CommandCenter />, ["/?tab=completed"]);
    await waitFor(() => expect(screen.getByText("shipped the thing")).toBeTruthy());
    // The local jump is an in-app note link; the remote jump opens the synced branch-notes repo.
    const local = screen.getByRole("link", { name: /note \(local\)/ });
    expect(local.getAttribute("href")).toContain("/notes?repo=bn&branch=feat");
    const remote = screen.getByRole("link", { name: /note \(remote\)/ });
    expect(remote.getAttribute("href")).toBe(
      "https://github.com/eduuh/branch-notes/blob/main/testhost/bn/feat/note.md",
    );
  });

  it("shows a graceful empty state when nothing is completed", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: string | URL) => {
        const u = String(url);
        if (u.startsWith("/api/completed"))
          return env("completed", { count: 0, completed: [] }) as unknown as Response;
        if (u.startsWith("/api/actions")) return env("actions", ACTIONS) as unknown as Response;
        if (u.startsWith("/api/fleet"))
          return env("fleet", { count: 0, agents: [] }) as unknown as Response;
        if (u.startsWith("/api/orchestrate"))
          return env("orchestrate", { count: 0, runs: [] }) as unknown as Response;
        if (u.startsWith("/api/reviews"))
          return env("reviews", { reviews: [] }) as unknown as Response;
        throw new Error(`unexpected fetch: ${u}`);
      }),
    );
    wrap(<CommandCenter />, ["/?tab=completed"]);
    await waitFor(() => expect(screen.getByText("Nothing completed yet")).toBeTruthy());
  });
});
