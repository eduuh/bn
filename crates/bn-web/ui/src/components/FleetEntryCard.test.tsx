// @vitest-environment jsdom
import { afterEach, describe, expect, it } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { FleetEntryCard } from "./FleetEntryCard";
import type { FleetEntry } from "../lib/fleet";

function entry(over: Partial<FleetEntry>): FleetEntry {
  return {
    key: "k",
    repo: "bn",
    branch: "dispatch/x",
    target: "bn:dispatch-x",
    role: "worker",
    stateLabel: "Working",
    statusLabel: "working",
    tone: "grey",
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
        <FleetEntryCard entry={e} now={0} />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

afterEach(cleanup);

describe("FleetEntryCard", () => {
  it("renders the needs-you reason on a needs card", () => {
    wrap(entry({ needs: true, needsReason: "Plan awaiting approval", reviewId: "r1", tone: "amber" }));
    expect(screen.getByText(/Plan awaiting approval/)).toBeTruthy();
  });

  it("omits the reason on a healthy card", () => {
    wrap(entry({ needs: false, needsReason: undefined, sessionId: "s1" }));
    expect(screen.queryByText(/awaiting/)).toBeNull();
  });

  it("does not render a reason line when a needs card carries no reason", () => {
    wrap(entry({ needs: true, needsReason: undefined, sessionId: "s1" }));
    expect(screen.queryByTestId("needs-reason")).toBeNull();
  });

  it("renders the orchestrator role badge on an orchestrator card", () => {
    wrap(entry({ role: "orchestrator", statusLabel: "orchestrator", sessionId: "s1" }));
    expect(screen.getByText(/Orchestrator/)).toBeTruthy();
  });

  it("omits the role badge on a worker card", () => {
    wrap(entry({ role: "worker", sessionId: "s1" }));
    expect(screen.queryByText(/Orchestrator/)).toBeNull();
  });

  // Branch-first hierarchy: on a single-repo fleet every card's repo is identical, so the branch
  // is the headline and the repo drops to the meta line.
  it("headlines the branch, not the repo", () => {
    wrap(entry({ branch: "fleets-view-improvement", repo: "bn", sessionId: "s1" }));
    const head = screen.getByText("fleets-view-improvement");
    expect(head.className).toContain("font-semibold");
    expect(screen.getByText("bn").className).not.toContain("font-semibold");
  });

  // The motion counters, not just spend — a token total alone can't distinguish an agent mid-edit
  // from one wedged an hour ago.
  it("renders turn and tool counters when the server carried them", () => {
    wrap(entry({ turns: 263, toolCalls: 160, outputTokens: 162_900, sessionId: "s1" }));
    expect(screen.getByText("263 turns")).toBeTruthy();
    expect(screen.getByText("160 tools")).toBeTruthy();
    expect(screen.getByText("162.9k tok")).toBeTruthy();
  });

  it("omits counters an older server did not send", () => {
    wrap(entry({ turns: undefined, toolCalls: undefined, sessionId: "s1" }));
    expect(screen.queryByText(/turns/)).toBeNull();
    expect(screen.queryByText(/tools/)).toBeNull();
  });

  // The harness is a chip that opens a menu, never a permanent block — and it needs a known
  // harness to label.
  it("renders the harness as a single chip, not a segmented block", () => {
    wrap(entry({ harness: "claude", sessionId: "s1" }));
    expect(screen.getByRole("button", { name: /claude/ })).toBeTruthy();
    expect(screen.queryByRole("button", { name: /copilot/ })).toBeNull();
  });

  it("renders no harness chip when the harness is unknown", () => {
    wrap(entry({ harness: undefined, sessionId: "s1" }));
    expect(screen.queryByRole("button", { name: /claude|copilot/ })).toBeNull();
  });

  // Repair is reachable from the board: a stuck card carries relaunch + the tmux jump.
  it("offers relaunch on a needs card only", () => {
    wrap(entry({ needs: true, needsReason: "No progress", control: "resume", sessionId: "s1" }));
    expect(screen.getByRole("button", { name: /Relaunch/ })).toBeTruthy();
    cleanup();
    wrap(entry({ needs: false, control: "resume", sessionId: "s1" }));
    expect(screen.queryByRole("button", { name: /Relaunch/ })).toBeNull();
  });

  it("disables relaunch when bn has no write channel to the agent", () => {
    wrap(entry({ needs: true, needsReason: "No progress", control: "none", sessionId: "s1" }));
    const btn = screen.getByRole("button", { name: /Relaunch/ }) as HTMLButtonElement;
    expect(btn.disabled).toBe(true);
  });
});
