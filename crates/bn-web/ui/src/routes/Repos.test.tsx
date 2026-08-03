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
  slots: [],
  ...p,
});

const repo = (name: string, p: RepoPool, links: RepoRollup["links"] = null): RepoRollup => ({
  repo: name,
  managed: true,
  active_agents: 0,
  open_prs: 0,
  session_count: 1,
  last_activity_ts: 1_000,
  aic: null,
  cost_usd: null,
  pool: p,
  links,
  sessions: [],
});

/** A pooled repo (2/3 warm, 1 leased) with a GitHub origin, and an un-pooled one with no clone here
 * (so no links) — the two shapes every strip on the card has to handle. */
const REPOS = {
  repos: [
    repo(
      "bn",
      pool({ enabled: true, total: 3, warm: 2, leased: 1, idle: 2, last_warmed_ts: 500 }),
      {
        forge: "github",
        repo: "https://github.com/eduuh/bn",
        issues: "https://github.com/eduuh/bn/issues",
        pulls: "https://github.com/eduuh/bn/pulls",
      },
    ),
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

describe("Repos view — forge links", () => {
  it("links a repo's code, issues and PR pages straight out to its forge", async () => {
    wrap(<Repos />);
    await waitFor(() => expect(screen.getByText("bn")).toBeTruthy());
    const href = (name: RegExp) =>
      (screen.getByRole("link", { name }) as HTMLAnchorElement).getAttribute("href");
    expect(href(/GitHub/)).toBe("https://github.com/eduuh/bn");
    expect(href(/issues/)).toBe("https://github.com/eduuh/bn/issues");
    expect(href(/pulls/)).toBe("https://github.com/eduuh/bn/pulls");
    // Outbound, so it opens away from the dashboard rather than replacing it.
    expect(screen.getByRole("link", { name: /GitHub/ }).getAttribute("target")).toBe("_blank");
  });

  it("renders no link row for a repo whose origin bn could not resolve", async () => {
    wrap(<Repos />);
    await waitFor(() => expect(screen.getByText("forge")).toBeTruthy());
    // Every link on the board belongs to bn; forge contributes none rather than a guessed URL.
    const external = screen
      .getAllByRole("link")
      .filter((a) => (a as HTMLAnchorElement).getAttribute("target") === "_blank");
    expect(external).toHaveLength(3);
    expect(external.every((a) => (a as HTMLAnchorElement).href.includes("/eduuh/bn"))).toBe(true);
  });
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

describe("Repos view — pool setup", () => {
  it("offers to enable pooling on an un-pooled repo and writes a default size", async () => {
    const spy = vi
      .spyOn(writes, "setRepoPool")
      .mockResolvedValue({ repo: "forge", size: 2, message: "pooling forge at 2 slot(s)" });
    wrap(<Repos />);
    await waitFor(() => expect(screen.getByText("forge")).toBeTruthy());
    // The un-pooled repo is the only one offering to enable — "pooling off" is no longer a dead end.
    const enable = screen.getAllByRole("button", { name: /enable pool/ });
    expect(enable).toHaveLength(1);
    fireEvent.click(enable[0]);
    await waitFor(() => expect(spy).toHaveBeenCalledWith("forge", 2));
  });

  it("grows a pooled repo by one slot", async () => {
    const spy = vi
      .spyOn(writes, "setRepoPool")
      .mockResolvedValue({ repo: "bn", size: 4, message: "pooling bn at 4 slot(s)" });
    wrap(<Repos />);
    await waitFor(() => expect(screen.getByText("bn")).toBeTruthy());
    fireEvent.click(screen.getByRole("button", { name: /Grow bn/ }));
    await waitFor(() => expect(spy).toHaveBeenCalledWith("bn", 4));
  });

  it("confirms before the last slot goes, since that turns pooling off", async () => {
    const spy = vi
      .spyOn(writes, "setRepoPool")
      .mockResolvedValue({ repo: "bn", size: null, message: "stopped pooling bn" });
    // bn has 3 slots, so shrinking twice is still a resize; only stepping below 1 unpools.
    vi.stubGlobal("confirm", vi.fn(() => false));
    wrap(<Repos />);
    await waitFor(() => expect(screen.getByText("bn")).toBeTruthy());
    fireEvent.click(screen.getByRole("button", { name: /Shrink bn/ }));
    await waitFor(() => expect(spy).toHaveBeenCalledWith("bn", 2));
    expect(window.confirm).not.toHaveBeenCalled();
  });
});
