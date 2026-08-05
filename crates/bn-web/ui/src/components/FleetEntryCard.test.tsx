// @vitest-environment jsdom
import { afterEach, describe, expect, it } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
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
  return render(
    <MemoryRouter>
      <FleetEntryCard entry={e} now={0} />
    </MemoryRouter>,
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
});
