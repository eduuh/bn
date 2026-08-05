// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { ReactNode } from "react";

import { MachineHealth } from "./MachineHealth";
import type { MachineView } from "../api/types";

/** Read envelope helper — bn reads unwrap `{ ok, data }`. */
const env = (data: unknown) => ({
  ok: true,
  json: async () => ({ ok: true, command: "machine", data }),
});

const HEALTHY: MachineView = {
  cpu: { load1: 1.0, load5: 1.0, load15: 1.0, cores: 8, load_per_core: 0.125 },
  ram: {
    total_mb: 16000,
    free_mb: 12000,
    used_mb: 4000,
    swap_total_mb: 2000,
    swap_used_mb: 0,
    used_frac: 0.25,
    swap_used_frac: 0,
  },
  saturation: { level: "ok", reasons: [] },
  agents: { active: 2 },
};

const SATURATED: MachineView = {
  cpu: { load1: 147, load5: 120, load15: 90, cores: 8, load_per_core: 18.375 },
  ram: {
    total_mb: 16000,
    free_mb: 500,
    used_mb: 15500,
    swap_total_mb: 4000,
    swap_used_mb: 3000,
    used_frac: 0.97,
    swap_used_frac: 0.75,
  },
  saturation: {
    level: "critical",
    reasons: ["load 147.00 ≥ 1×cores (8)", "swap 75% used (OOM risk)", "RAM 97% used"],
  },
  agents: { active: 5 },
};

function wrap(node: ReactNode) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(<QueryClientProvider client={qc}>{node}</QueryClientProvider>);
}

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("MachineHealth (#525)", () => {
  it("shows the loading placeholder while the query is pending", () => {
    vi.stubGlobal("fetch", vi.fn(() => new Promise<Response>(() => {})));
    wrap(<MachineHealth />);
    expect(screen.getByText(/Loading machine health/)).toBeTruthy();
  });

  it("renders a healthy snapshot with no saturation warning", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => env(HEALTHY) as unknown as Response),
    );
    wrap(<MachineHealth />);

    await waitFor(() => expect(screen.getByText("healthy")).toBeTruthy());
    // CPU / RAM detail + active-agent footprint render.
    expect(screen.getByText(/1.00 \/ 8 cores/)).toBeTruthy();
    expect(screen.getByText(/2 agents loading it/)).toBeTruthy();
    // No swap in use → no reasons banner.
    expect(screen.queryByText(/OOM risk/)).toBeNull();
  });

  it("surfaces the near-saturation warning + reasons when the host is melting", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => env(SATURATED) as unknown as Response),
    );
    wrap(<MachineHealth />);

    await waitFor(() => expect(screen.getByText("saturated")).toBeTruthy());
    // The reasons string (the same one #503 backpressure uses) is shown to the human.
    expect(screen.getByText(/swap 75% used \(OOM risk\)/)).toBeTruthy();
    expect(screen.getByText(/5 agents loading it/)).toBeTruthy();
  });

  it("labels a machine with no swap configured as 'none'", async () => {
    const noSwap: MachineView = {
      ...HEALTHY,
      ram: { ...HEALTHY.ram, swap_total_mb: 0, swap_used_mb: 0, swap_used_frac: null },
    };
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => env(noSwap) as unknown as Response),
    );
    wrap(<MachineHealth />);

    await waitFor(() => expect(screen.getByText("healthy")).toBeTruthy());
    expect(screen.getByText("none")).toBeTruthy();
  });
});
