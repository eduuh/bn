// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import type { ReactNode } from "react";

// uPlot draws to a canvas jsdom does not implement; the chart's own model-building is covered by
// `lib/cost.test.ts`, so stub the canvas and keep this test about the page composition.
vi.mock("../components/CostChart", () => ({
  CostChart: () => <div data-testid="cost-chart" />,
}));

import { Insights } from "./Insights";

const METRICS = {
  window_days: 30,
  pr: {
    window_days: 30,
    opened: 9,
    merged: 7,
    closed: 1,
    in_flight: 2,
    completion_rate: 0.875,
    cycle_median_hours: 3.5,
    cycle_p90_hours: 12,
    lead_median_hours: null,
    lead_p90_hours: null,
  },
  wip: 4,
  open_branches: 6,
  branch_age_median_days: 2,
  branch_age_max_days: 9,
  agent: {
    sessions: 12,
    total_minutes: 130,
    by_harness: [["copilot", 12]],
    by_model: [["claude-opus", 12]],
    cost_aiu: 42.5,
    input_tokens: 1000,
    output_tokens: 500,
    lines_added: 300,
    lines_removed: 120,
  },
};

const TREND = {
  snapshots: [
    { date: "2026-07-01", wip: 3, merged: 2, close_out_lag_median_hours: 4 },
    { date: "2026-07-08", wip: 4, merged: 5, close_out_lag_median_hours: 3 },
    { date: "2026-07-15", wip: 4, merged: 7, close_out_lag_median_hours: 2 },
  ],
};

const row = (key: string) => ({
  key,
  sessions: 3,
  input_tokens: 900,
  cache_read_tokens: 100,
  output_tokens: 400,
  aic: 12.5,
  cost_usd: 4.25,
});

const COST = {
  window_days: 30,
  totals: {
    key: "total",
    sessions: 6,
    input_tokens: 1800,
    cache_read_tokens: 200,
    output_tokens: 800,
    aic: 25,
    cost_usd: 8.5,
  },
  by_category: [row("coding")],
  by_machine: [row("mbp")],
  by_repo: [row("bn")],
  by_model: [row("claude-opus")],
  by_day: [row("2026-07-15")],
  by_day_category: [
    {
      day: "2026-07-15",
      category: "coding",
      sessions: 6,
      input_tokens: 1800,
      cache_read_tokens: 200,
      output_tokens: 800,
      aic: 25,
      cost_usd: 8.5,
    },
  ],
};

const FLEET = { agents: [], active: 0, idle: 0, stuck: 0, total_tokens: 0 };

const env = (command: string, data: unknown) => ({
  ok: true,
  json: async () => ({ ok: true, command, data }),
});

function wrap(node: ReactNode, entries: string[] = ["/insights"]) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={qc}>
      <MemoryRouter initialEntries={entries}>{node}</MemoryRouter>
    </QueryClientProvider>,
  );
}

beforeEach(() => {
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string | URL) => {
      const u = String(url);
      if (u.startsWith("/api/metrics/trend")) return env("metrics_trend", TREND) as unknown as Response;
      if (u.startsWith("/api/metrics")) return env("metrics", METRICS) as unknown as Response;
      if (u.startsWith("/api/weekly")) return env("weekly", { rendered: "WEEKLY DIGEST BODY" }) as unknown as Response;
      if (u.startsWith("/api/insights")) return env("insights", { rendered: "INSIGHTS RETRO BODY" }) as unknown as Response;
      if (u.startsWith("/api/brag")) return env("brag", { rendered: "# BRAG PACKET BODY" }) as unknown as Response;
      if (u.startsWith("/api/cost")) return env("cost", COST) as unknown as Response;
      if (u.startsWith("/api/fleet")) return env("fleet", FLEET) as unknown as Response;
      throw new Error(`unexpected fetch: ${u}`);
    }),
  );
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("Insights & Cost view (#507)", () => {
  it("renders the metrics tiles and the trend chart", async () => {
    wrap(<Insights />);
    // PR merged tile value + completion rate + agent AIC.
    await waitFor(() => expect(screen.getByText("PRs merged")).toBeTruthy());
    expect(screen.getByText("7")).toBeTruthy();
    expect(screen.getByText("3 snapshots")).toBeTruthy();
    // The small chart renders once there are ≥2 snapshots.
    expect(screen.getByRole("img", { name: /merged PRs per snapshot/ })).toBeTruthy();
  });

  it("switches to the Brag digest tab and shows the rendered packet", async () => {
    wrap(<Insights />);
    await waitFor(() => expect(screen.getByText("PRs merged")).toBeTruthy());
    const bragTab = screen.getByRole("tab", { name: "Brag" });
    // Radix Tabs use focus activation — focus + click to switch (jsdom quirk).
    fireEvent.focus(bragTab);
    fireEvent.click(bragTab);
    await waitFor(() => expect(screen.getByText(/BRAG PACKET BODY/)).toBeTruthy());
  });

  // Cost was its own `/cost` route until the two reporting screens merged; it is now a tab here,
  // fed by *this* page's window picker rather than a second one that could disagree with it.
  it("shows the cost tiles on the Cost tab", async () => {
    wrap(<Insights />);
    await waitFor(() => expect(screen.getByText("PRs merged")).toBeTruthy());

    const costTab = screen.getByRole("tab", { name: "Cost" });
    fireEvent.focus(costTab);
    fireEvent.click(costTab);

    await waitFor(() => expect(screen.getByText("AIC · Copilot bill")).toBeTruthy());
    expect(screen.getByText("$ · API-equiv")).toBeTruthy();
    expect(screen.getByTestId("cost-chart")).toBeTruthy();
  });

  it("deep-links the cost tab from ?tab=cost", async () => {
    wrap(<Insights />, ["/insights?tab=cost"]);
    await waitFor(() => expect(screen.getByText("AIC · Copilot bill")).toBeTruthy());
    // The metrics panel is the default tab, so its absence proves the URL drove the selection.
    expect(screen.queryByText("PRs merged")).toBeNull();
  });
});
