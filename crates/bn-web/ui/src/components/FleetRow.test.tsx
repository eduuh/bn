// @vitest-environment jsdom
import { afterEach, describe, expect, it } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { FleetRow } from "./FleetRow";
import type { FleetEntry } from "../lib/fleet";

function entry(over: Partial<FleetEntry> = {}): FleetEntry {
  return {
    key: "k",
    repo: "bn",
    branch: "fleets-view-improvement",
    target: "bn:fleets-view-improvement",
    role: "worker",
    stateLabel: "Active",
    statusLabel: "active",
    tone: "green",
    updatedAt: 0,
    needs: false,
    ...over,
  };
}

function wrap(e: FleetEntry) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={qc}>
      <MemoryRouter>
        <FleetRow entry={e} now={0} />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

afterEach(cleanup);

describe("FleetRow", () => {
  it("leads with the branch and carries the state and counters", () => {
    wrap(entry({ turns: 51, toolCalls: 32, outputTokens: 26_700, sessionId: "s1" }));
    expect(screen.getByText("fleets-view-improvement")).toBeTruthy();
    expect(screen.getByText("Active")).toBeTruthy();
    expect(screen.getByText("51 turns")).toBeTruthy();
    expect(screen.getByText("26.7k tok")).toBeTruthy();
  });

  it("taps through to the agent detail when a live session backs it", () => {
    wrap(entry({ sessionId: "abc-123" }));
    const link = screen.getByRole("link");
    expect(link.getAttribute("href")).toBe("/agents/abc-123");
  });

  it("routes a plan-review row to the reviews inbox instead", () => {
    wrap(entry({ sessionId: "abc", reviewId: "r1" }));
    expect(screen.getByRole("link").getAttribute("href")).toBe("/?tab=reviews");
  });

  it("stays a static row when no session backs it", () => {
    wrap(entry({ sessionId: undefined }));
    expect(screen.queryByRole("link")).toBeNull();
  });

  it("carries the harness chip at row scale", () => {
    wrap(entry({ harness: "claude", sessionId: "s1" }));
    expect(screen.getByRole("button", { name: /claude/ })).toBeTruthy();
  });
});
