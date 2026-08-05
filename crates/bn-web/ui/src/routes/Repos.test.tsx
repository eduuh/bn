// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import type { ReactNode } from "react";
import { Repos } from "./Repos";
import type { RepoPool, RepoRollup } from "../api/types";
import * as writes from "../api/writes";

const pool = (p: Partial<RepoPool>): RepoPool => ({
  enabled: false,
  total: 0,
  warm: 0,
  leased: 0,
  idle: 0,
  last_warmed_ts: null,
  ...p,
});

const repo = (name: string, p: RepoPool): RepoRollup => ({
  repo: name,
  managed: true,
  active_agents: 0,
  open_prs: 0,
  session_count: 1,
  last_activity_ts: 1_000,
  aic: null,
  cost_usd: null,
  pool: p,
  sessions: [],
});

/** A pooled repo (2/3 warm, 1 leased) and an un-pooled one. */
const REPOS = {
  repos: [
    repo("bn", pool({ enabled: true, total: 3, warm: 2, leased: 1, idle: 2, last_warmed_ts: 500 })),
    repo("forge", pool({ enabled: false })),
  ],
};

const env = (data: unknown) => ({
  ok: true,
  json: async () => ({ ok: true, command: "repos", data }),
});

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
      if (u.startsWith("/api/repos")) return env(REPOS) as unknown as Response;
      throw new Error(`unexpected fetch: ${u}`);
    }),
  );
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("Repos view — warm pool (#497)", () => {
  it("shows the warm / leased indicator for a pooled repo and 'pooling off' for an un-pooled one", async () => {
    wrap(<Repos />);
    await waitFor(() => expect(screen.getByText("bn")).toBeTruthy());
    // Pooled repo: warm ratio + leased count.
    expect(screen.getByText(/2\/3 warm/)).toBeTruthy();
    expect(screen.getByText(/1 leased/)).toBeTruthy();
    // Un-pooled repo: an explicit disabled marker (no flashed zeros).
    expect(screen.getByText(/pooling off/)).toBeTruthy();
  });

  it("renders a Warm button only for the pooled repo", async () => {
    wrap(<Repos />);
    await waitFor(() => expect(screen.getByText("bn")).toBeTruthy());
    // Exactly one warm button (bn), none for the un-pooled forge.
    expect(screen.getAllByRole("button", { name: /warm pool/ })).toHaveLength(1);
  });

  it("POSTs the warm and does not navigate when clicked", async () => {
    const spy = vi.spyOn(writes, "warmRepoPool").mockResolvedValue({
      repo: "bn",
      status: "started",
      pid: 1,
      message: "warming pool for bn",
    });
    vi.stubGlobal("confirm", vi.fn(() => true));
    wrap(<Repos />);
    await waitFor(() => expect(screen.getByText("bn")).toBeTruthy());
    fireEvent.click(screen.getByRole("button", { name: /warm pool/ }));
    await waitFor(() => expect(spy).toHaveBeenCalledWith("bn"));
  });

  it("does not warm when the confirm is dismissed", async () => {
    const spy = vi.spyOn(writes, "warmRepoPool").mockResolvedValue({
      repo: "bn",
      status: "started",
      pid: 1,
      message: "x",
    });
    vi.stubGlobal("confirm", vi.fn(() => false));
    wrap(<Repos />);
    await waitFor(() => expect(screen.getByText("bn")).toBeTruthy());
    fireEvent.click(screen.getByRole("button", { name: /warm pool/ }));
    expect(spy).not.toHaveBeenCalled();
  });
});
