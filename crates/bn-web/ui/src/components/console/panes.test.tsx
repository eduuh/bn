// @vitest-environment jsdom
//
// Where an agent's controls live in the console (#1045). The hermetic E2E fixture seeds actions and
// plan reviews but no *live* agent, so the agent branch of these two panes has no queue row to open
// there — it is covered here instead.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import type { ReactNode } from "react";
import type { WorkItem } from "../../lib/console";
import { ActPane } from "./ActPane";
import { FocusPane } from "./FocusPane";

const AGENT_ITEM: WorkItem = {
  id: "agent:live/s1",
  lane: "running",
  verb: "watch",
  kind: "Agent",
  title: "bn/dispatch/steer-inline",
  where: "bn/dispatch/steer-inline",
  tone: "ok",
  updatedAt: 1_000,
  agent: {
    key: "bn:dispatch/steer-inline",
    repo: "bn",
    branch: "dispatch/steer-inline",
    target: "bn:dispatch/steer-inline",
    sessionId: "s1",
    harness: "claude",
    role: "worker",
    stateLabel: "Working",
    statusLabel: "active",
    tone: "green",
    updatedAt: 1_000,
    needs: false,
    control: "resume",
  },
};

function wrap(node: ReactNode) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={qc}>
      <MemoryRouter>{node}</MemoryRouter>
    </QueryClientProvider>,
  );
}

describe("console panes — an agent's controls", () => {
  beforeEach(() => {
    // Both panes fetch (activity, branch note, reviews); an empty envelope is enough to render.
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => ({
        ok: true,
        json: async () => ({ ok: true, command: "stub", data: {} }),
      })),
    );
  });
  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
  });

  it("puts the steer composer in the focus pane, under the transcript it answers", () => {
    wrap(<FocusPane item={AGENT_ITEM} blocked={0} now={2_000} />);
    expect(screen.getByRole("heading", { name: "Steer" })).toBeTruthy();
    expect(screen.getByPlaceholderText(/Message the agent/)).toBeTruthy();
  });

  it("keeps a half-typed message when you read the branch note and come back", () => {
    wrap(<FocusPane item={AGENT_ITEM} blocked={0} now={2_000} />);
    const box = screen.getByPlaceholderText(/Message the agent/) as HTMLTextAreaElement;
    fireEvent.change(box, { target: { value: "half-typed" } });

    fireEvent.click(screen.getByRole("button", { name: "Note" }));
    // The composer is rendered outside the tab switch, so the tab change never unmounts it.
    expect((screen.getByPlaceholderText(/Message the agent/) as HTMLTextAreaElement).value).toBe(
      "half-typed",
    );
  });

  it("leaves steering and landing out of the act column", () => {
    wrap(<ActPane item={AGENT_ITEM} />);
    expect(screen.queryByRole("heading", { name: "Steer" })).toBeNull();
    expect(screen.queryByRole("button", { name: /Land ready PRs/ })).toBeNull();
  });
});
