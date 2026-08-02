// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import type { ReactNode } from "react";
import { Nodes } from "./Nodes";

/** Read envelope helper — bn reads unwrap `{ ok, data }`. */
const env = (data: unknown) => ({
  ok: true,
  json: async () => ({ ok: true, command: "nodes", data }),
});

const NODE = {
  host: "mac.lan",
  platform: "macos",
  arch: "aarch64",
  status: "online",
  local: true,
  cpu: { load1: 2.01, load5: 1.8, load15: 1.5, cores: 10, load_per_core: 0.2 },
  ram: {
    total_mb: 16384,
    free_mb: 5800,
    used_mb: 10584,
    swap_total_mb: 0,
    swap_used_mb: 0,
    used_frac: 0.64,
    swap_used_frac: null,
  },
  saturation: { level: "ok", reasons: [] },
  agents: { active: 3 },
};

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

describe("Nodes", () => {
  it("renders the local node card from /api/nodes", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: string) => {
        if (url.startsWith("/api/nodes")) return env({ nodes: [NODE] });
        throw new Error(`unexpected fetch: ${url}`);
      }),
    );
    wrap(<Nodes />);
    await waitFor(() => expect(screen.getByText("mac.lan")).toBeTruthy());
    expect(screen.getByText("healthy")).toBeTruthy();
    expect(screen.getByText("local")).toBeTruthy();
    expect(screen.getByText(/darwin · aarch64/)).toBeTruthy();
    expect(screen.getByText(/3 agents loading it/)).toBeTruthy();
    // Single node → the remote-nodes explainer rides along.
    expect(screen.getByText(/Remote nodes appear here automatically/)).toBeTruthy();
  });

  it("renders an unreachable node without vitals, in the signal scale", async () => {
    const down = { ...NODE, host: "win-mt5", status: "unreachable", local: false };
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: string) => {
        if (url.startsWith("/api/nodes")) return env({ nodes: [NODE, down] });
        throw new Error(`unexpected fetch: ${url}`);
      }),
    );
    wrap(<Nodes />);
    await waitFor(() => expect(screen.getByText("win-mt5")).toBeTruthy());
    expect(screen.getByText("unreachable")).toBeTruthy();
    expect(screen.getByText(/no heartbeat — vitals unavailable/)).toBeTruthy();
    expect(screen.getByText(/1 up · 1 unreachable/)).toBeTruthy();
    // Gauges render only for the healthy node.
    expect(screen.getAllByText(/CPU load/)).toHaveLength(1);
  });
});
