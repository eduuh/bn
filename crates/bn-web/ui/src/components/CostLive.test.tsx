// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { ReactNode } from "react";
import { CostLive } from "./CostLive";

/** Read envelope helper — bn reads unwrap `{ ok, data }`. */
const env = (data: unknown) => ({
  ok: true,
  json: async () => ({ ok: true, command: "fleet", data }),
});

const EMPTY_FLEET = { agents: [], active: 0, idle: 0, stuck: 0, total_tokens: 0 };

function wrap(node: ReactNode) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(<QueryClientProvider client={qc}>{node}</QueryClientProvider>);
}

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("CostLive cold load (#544)", () => {
  it("shows the loading placeholder — not the 0-token / empty state — while the fleet query is pending", () => {
    // A fetch that never resolves keeps the query in isLoading:true / data:undefined.
    vi.stubGlobal("fetch", vi.fn(() => new Promise<Response>(() => {})));
    wrap(<CostLive />);

    expect(screen.getByText(/Loading live usage/)).toBeTruthy();
    // The misleading cold-load states must be absent until data resolves.
    expect(screen.queryByText(/No agents currently running/)).toBeNull();
    expect(screen.queryByText(/in-flight tokens/)).toBeNull();
  });

  it("renders the real in-flight tiles + empty note only after the fleet query resolves", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => env(EMPTY_FLEET) as unknown as Response),
    );
    wrap(<CostLive />);

    await waitFor(() => expect(screen.getByText(/No agents currently running/)).toBeTruthy());
    expect(screen.getByText(/in-flight tokens/)).toBeTruthy();
    expect(screen.queryByText(/Loading live usage/)).toBeNull();
  });
});
