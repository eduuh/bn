// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import type { ReactNode } from "react";
import { CronDetail } from "./CronDetail";
import * as writes from "../api/writes";

/** Envelope helper — reads unwrap `{ ok, data }`. */
const env = (command: string, data: unknown) => ({
  ok: true,
  json: async () => ({ ok: true, command, data }),
});

const CRON = {
  count: 1,
  jobs: [
    {
      job: "gh-prs",
      scope: "global",
      declared: true,
      schedule: "*:0/15",
      description: "Sweep open PRs",
      network: true,
      catch_up: false,
      has_script: true,
      runs: 4,
      failures: 1,
      last_run: 1_000,
      last_exit: 1,
      ok: false,
      last_output: "boom: it broke\n  at step 3",
    },
  ],
};

const FINDINGS = {
  count: 1,
  runs: [
    {
      source: "gh-prs",
      run_id: "run-1",
      gathered_at: 2_000,
      items: [
        { key: "pr-1", text: "Fix the flaky test", url: "https://example.test/pr/1" },
        { key: "pr-2", text: "Bump dep", url: "https://example.test/pr/2" },
      ],
    },
  ],
};

function wrap(node: ReactNode) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={qc}>
      <MemoryRouter initialEntries={["/cron/gh-prs"]}>
        <Routes>
          <Route path="/cron/:id" element={node} />
        </Routes>
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

beforeEach(() => {
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string | URL) => {
      const u = String(url);
      if (u.startsWith("/api/findings")) return env("findings", FINDINGS) as unknown as Response;
      if (u.startsWith("/api/cron")) return env("cron", CRON) as unknown as Response;
      throw new Error(`unexpected fetch: ${u}`);
    }),
  );
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("CronDetail (#579)", () => {
  it("renders the identity header, schedule, and last-run failure + output tail", async () => {
    wrap(<CronDetail />);
    await waitFor(() => expect(screen.getByRole("heading", { name: "gh-prs" })).toBeTruthy());
    expect(screen.getByText("*:0/15")).toBeTruthy();
    expect(screen.getByText(/Last run failed/)).toBeTruthy();
    expect(screen.getByText(/boom: it broke/)).toBeTruthy();
  });

  it("renders the stored findings with their deep links", async () => {
    wrap(<CronDetail />);
    await waitFor(() => expect(screen.getByText("Fix the flaky test")).toBeTruthy());
    const link = screen.getByRole("link", { name: /Fix the flaky test/ });
    expect(link.getAttribute("href")).toBe("https://example.test/pr/1");
    expect(screen.getByText("Bump dep")).toBeTruthy();
  });

  it("fires run-now via POST /api/cron/run", async () => {
    const spy = vi.spyOn(writes, "runCronJob").mockResolvedValue(undefined as never);
    vi.stubGlobal("confirm", vi.fn(() => true));
    wrap(<CronDetail />);
    await waitFor(() => expect(screen.getByRole("heading", { name: "gh-prs" })).toBeTruthy());
    fireEvent.click(screen.getByRole("button", { name: /run now/ }));
    await waitFor(() => expect(spy).toHaveBeenCalledWith("gh-prs"));
  });

  it("shows a not-found card for an unknown job", async () => {
    const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    render(
      <QueryClientProvider client={qc}>
        <MemoryRouter initialEntries={["/cron/does-not-exist"]}>
          <Routes>
            <Route path="/cron/:id" element={<CronDetail />} />
          </Routes>
        </MemoryRouter>
      </QueryClientProvider>,
    );
    await waitFor(() => expect(screen.getByText(/Cron job not found/)).toBeTruthy());
  });
});
