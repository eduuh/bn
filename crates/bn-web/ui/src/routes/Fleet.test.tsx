// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import type { ReactNode } from "react";
import { Fleet } from "./Fleet";

/** Read envelope helper — bn reads unwrap `{ ok, data }`. */
const env = (data: unknown) => ({
  ok: true,
  json: async () => ({ ok: true, command: "fleet", data }),
});

const EMPTY_ORCH = { runs: [] };
const EMPTY_FLEET = { agents: [], active: 0, idle: 0, stuck: 0, total_tokens: 0 };
const EMPTY_RECOVERY = { recovery: [] };

function reply(url: string) {
  if (url.startsWith("/api/orchestrate/recovery")) return env(EMPTY_RECOVERY);
  if (url.startsWith("/api/orchestrate")) return env(EMPTY_ORCH);
  if (url.startsWith("/api/fleet")) return env(EMPTY_FLEET);
  throw new Error(`unexpected fetch: ${url}`);
}

function wrap(node: ReactNode) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={qc}>
      <MemoryRouter>{node}</MemoryRouter>
    </QueryClientProvider>,
  );
}

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("Fleet cold load (#544)", () => {
  it("shows the loading state — not the '0 running / no agents' empty state — while a query is pending", () => {
    // A fetch that never resolves keeps both queries in isLoading:true / data:undefined; the OR
    // (not AND) guard means we stay in the loading state instead of flashing the empty state.
    vi.stubGlobal("fetch", vi.fn(() => new Promise<Response>(() => {})));
    wrap(<Fleet />);

    expect(screen.getByText(/Loading fleet/)).toBeTruthy();
    expect(screen.queryByText(/No agents running/)).toBeNull();
    // The header count must not flash "0 running" during the cold load.
    expect(screen.queryByText(/0 running/)).toBeNull();
  });

  it("shows the empty state only after both queries resolve empty", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: string | URL) => reply(String(url)) as unknown as Response),
    );
    wrap(<Fleet />);

    await waitFor(() => expect(screen.getByText(/No agents running/)).toBeTruthy());
    expect(screen.queryByText(/Loading fleet/)).toBeNull();
  });

  it("renders a pending 'Starting…' tier for just-dispatched agents (#545)", async () => {
    // Two just-dispatched agents with no transcript yet — `starting` drives a pending tier, and
    // the presence of pending work must keep us out of the 'No agents running' empty state.
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: string | URL) => {
        const u = String(url);
        if (u.startsWith("/api/orchestrate/recovery")) return env(EMPTY_RECOVERY) as unknown as Response;
        if (u.startsWith("/api/orchestrate")) return env(EMPTY_ORCH) as unknown as Response;
        if (u.startsWith("/api/fleet"))
          return env({ ...EMPTY_FLEET, starting: 2 }) as unknown as Response;
        throw new Error(`unexpected fetch: ${u}`);
      }),
    );
    wrap(<Fleet />);

    await waitFor(() =>
      expect(screen.getByRole("heading", { name: /Starting…/ })).toBeTruthy(),
    );
    expect(screen.queryByText(/No agents running/)).toBeNull();
  });
});
