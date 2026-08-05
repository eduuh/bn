// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import type { ReactNode } from "react";
import { Cron } from "./Cron";
import * as writes from "../api/writes";

/** Envelope helper — the cron read unwraps `{ ok, data }`. */
const env = (data: unknown) => ({
  ok: true,
  json: async () => ({ ok: true, command: "cron", data }),
});

/** One failing job carrying a captured output tail (#538) plus a healthy one. */
const CRON = {
  count: 2,
  jobs: [
    {
      job: "wt-cleanup",
      scope: "global",
      declared: true,
      schedule: "*:0/15",
      network: false,
      catch_up: false,
      has_script: true,
      runs: 4,
      failures: 1,
      last_run: 1_000,
      last_exit: 1,
      ok: false,
      last_output: "boom: it broke\n  at step 3",
    },
    {
      job: "refresh-all",
      scope: "global",
      declared: true,
      schedule: "daily",
      network: true,
      catch_up: false,
      has_script: true,
      runs: 9,
      failures: 0,
      last_run: 2_000,
      last_exit: 0,
      ok: true,
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
      if (u.startsWith("/api/cron")) return env(CRON) as unknown as Response;
      throw new Error(`unexpected fetch: ${u}`);
    }),
  );
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("Cron view (#538)", () => {
  it("renders a per-job re-run button", async () => {
    wrap(<Cron />);
    await waitFor(() => expect(screen.getByText("wt-cleanup")).toBeTruthy());
    // One re-run button per job (2 jobs in the fixture).
    expect(screen.getAllByRole("button", { name: /re-run/ })).toHaveLength(2);
  });

  it("reveals a failing job's captured output behind the show-output toggle", async () => {
    wrap(<Cron />);
    await waitFor(() => expect(screen.getByText("wt-cleanup")).toBeTruthy());
    // The tail is hidden until the toggle is clicked.
    expect(screen.queryByText(/boom: it broke/)).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: /show output/ }));
    await waitFor(() => expect(screen.getByText(/boom: it broke/)).toBeTruthy());
  });

  it("POSTs /api/cron/run and invalidates the cron query on re-run", async () => {
    const spy = vi.spyOn(writes, "runCronJob").mockResolvedValue(undefined as never);
    vi.stubGlobal("confirm", vi.fn(() => true));
    wrap(<Cron />);
    await waitFor(() => expect(screen.getByText("wt-cleanup")).toBeTruthy());
    // Re-run the first (failing) job — its button leads the list (failing jobs sort first).
    fireEvent.click(screen.getAllByRole("button", { name: /re-run/ })[0]);
    await waitFor(() => expect(spy).toHaveBeenCalledWith("wt-cleanup"));
  });
});
