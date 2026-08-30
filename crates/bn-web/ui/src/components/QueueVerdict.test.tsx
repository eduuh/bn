import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import { QueueVerdict } from "./QueueVerdict";
import type { QueueMachine, QueueNodeRow, QueueView } from "../api/queue-types";

/**
 * These fixtures are the live `/api/queue` payload from a machine that had been sitting on ten
 * undispatched entries: dispatcher up, queue accepting, a free worker slot, and nothing moving.
 * That state is the whole reason this card exists, so it is the state the tests are written
 * against.
 */

const NODE: QueueNodeRow = {
  node_id: "mac.lan",
  target: "local",
  status: "ready",
  reported_status: "ready",
  stale: false,
  last_heartbeat: 1_786_385_000,
  heartbeat_age_secs: 12,
  slots: 1,
  busy: 0,
  free: 1,
  skills: [],
  harnesses: ["claude"],
  labels: [],
  owned_repos: [],
  ram_total_mb: 16_384,
  ram_reserved_mb: 4_096,
  capacity: null,
};

function machine(overrides: Partial<QueueMachine> = {}): QueueMachine {
  return {
    dispatcher_alive: true,
    dispatcher_pass_age_secs: 4,
    captured: [],
    lanes: [
      { lane: "coding", cap: 5, used: 2 },
      { lane: "research", cap: 2, used: 0 },
    ],
    lanes_why: "derived from RAM and cores",
    builds: { cap: 2, held: [] },
    windows: [],
    presence: { idle: false, away_window: false, opportunistic_idle_mins: 10 },
    ...overrides,
  };
}

function stalled(overrides: Partial<QueueView> = {}): QueueView {
  const now = 1_786_385_426;
  return {
    now,
    accepting: true,
    pending: 2,
    kinds: [{ kind: "ship", lane: "coding", pending: 2 }],
    nodes: [NODE],
    machine: machine(),
    entries: [
      {
        entry: {
          id: "you-are-the-repo-setup-agent",
          repo: "nvim",
          item: "You are the repo setup agent for `nvim`… warm pool preparation…",
          kind: "ship",
          priority: 0,
          status: "pending",
          added_at: now - 3_600,
          updated_at: now - 3_600,
        },
        // What the dispatcher derived — not what anyone asked for. Surfacing this is the point.
        class: {
          mode: "opportunistic",
          subtype: "warming",
          autonomous: true,
          opportunistic: true,
        },
      },
      {
        entry: {
          id: "warm-pool-for-bash",
          repo: "machine",
          item: "warm the bash pool",
          kind: "cron",
          priority: 0,
          status: "pending",
          added_at: now - 900,
          updated_at: now - 900,
        },
        class: {
          mode: "opportunistic",
          subtype: "warming",
          autonomous: true,
          opportunistic: true,
        },
      },
    ],
    decision: {
      at: now - 60,
      outcome: "started nothing",
      dry_run: false,
      pending: 2,
      started: 0,
      deferred: [
        {
          repo: "nvim",
          id: "you-are-the-repo-setup-agent",
          kind: "ship",
          reason: "awaiting-away",
          detail: "waiting for idle time or an away window",
          waiting_since: now - 3_600,
        },
        {
          repo: "machine",
          id: "warm-pool-for-bash",
          kind: "cron",
          reason: "awaiting-away",
          detail: "waiting for idle time or an away window",
          waiting_since: now - 900,
        },
      ],
    },
    ...overrides,
  };
}

// The suite runs without vitest `globals`, so testing-library never registers its own teardown
// and each render would stack another copy of the card into the same document.
afterEach(cleanup);

describe("QueueVerdict", () => {
  it("renders a disabled queue as not enabled rather than offline or accepting", () => {
    render(
      <QueueVerdict
        data={{
          ...stalled(),
          state: "not_enabled",
          message: "queue is not enabled on this machine ('web-only')",
          accepting: false,
          pending: 0,
          entries: [],
          nodes: [],
          machine: undefined,
        }}
        loading={false}
        error={false}
      />,
    );

    expect(screen.getByText("Queue is not enabled")).toBeTruthy();
    expect(screen.getByText("not enabled")).toBeTruthy();
    expect(screen.queryByText("The dispatcher is offline")).toBeNull();
    expect(screen.queryByText("accepting")).toBeNull();
  });

  it("names presence as the blocker instead of only labelling the symptom", () => {
    // The old card rendered two rows reading `awaiting-away` and never said what the machine was
    // waiting for, which is the one fact that explains the stall.
    render(<QueueVerdict data={stalled()} loading={false} error={false} />);

    expect(screen.getByText(/Nothing started/)).toBeTruthy();
    expect(screen.getByText(/Blocked on presence/)).toBeTruthy();
    expect(screen.getByText(/10m pass with no recorded activity/)).toBeTruthy();
  });

  it("collapses repeated hold reasons into one counted row", () => {
    render(<QueueVerdict data={stalled()} loading={false} error={false} />);

    const group = screen.getByRole("button", { name: /awaiting-away/ });
    expect(within(group).getByText("2")).toBeTruthy();
    expect(
      within(group).getByText(/only starts once the machine is idle or inside an away window/),
    ).toBeTruthy();
  });

  it("shows each held entry's derived class, so a misclassification is visible", () => {
    // A repo-setup agent classed `opportunistic · warming` is a bug in the derivation, and the
    // page is where that is noticed — the entry itself looks perfectly ordinary.
    render(<QueueVerdict data={stalled()} loading={false} error={false} />);

    fireEvent.click(screen.getByRole("button", { name: /awaiting-away/ }));

    expect(screen.getByText("nvim/you-are-the-repo-setup-agent")).toBeTruthy();
    expect(screen.getAllByText("opportunistic · warming").length).toBe(2);
  });

  it("stops blaming presence once the machine goes idle", () => {
    const idle = stalled({
      machine: machine({
        presence: { idle: true, away_window: false, opportunistic_idle_mins: 10 },
      }),
    });
    render(<QueueVerdict data={idle} loading={false} error={false} />);

    expect(screen.getByText(/quiet long enough/)).toBeTruthy();
    expect(screen.queryByText(/Blocked on presence/)).toBeNull();
  });

  it("calls a saturated lane out by name", () => {
    const full = stalled({
      machine: machine({
        lanes: [
          { lane: "coding", cap: 5, used: 6 },
          { lane: "research", cap: 2, used: 0 },
        ],
      }),
    });
    render(<QueueVerdict data={full} loading={false} error={false} />);

    expect(screen.getByText("coding full")).toBeTruthy();
    expect(screen.getByText(/Blocked on presence and lanes/)).toBeTruthy();
  });

  it("leads with the dispatcher when it is not even running", () => {
    // Every other number on the page is meaningless in this state; say so first.
    const dead = stalled({ machine: machine({ dispatcher_alive: false }) });
    render(<QueueVerdict data={dead} loading={false} error={false} />);

    expect(screen.getByText("The dispatcher is offline")).toBeTruthy();
  });

  it("tells a wedged dispatcher apart from a dead one", () => {
    // The daemon heartbeats from its own thread, so a process blocked inside a launch reports
    // alive forever. Only the age of the last completed pass distinguishes the two, and the
    // difference is what the reader does next: restart it, or go find what it is stuck on.
    const wedged = stalled({
      machine: machine({ dispatcher_alive: true, dispatcher_pass_age_secs: 1_500 }),
    });
    render(<QueueVerdict data={wedged} loading={false} error={false} />);

    expect(screen.getByText("The dispatcher is stuck")).toBeTruthy();
    expect(screen.getByText(/no pass has finished in 25m/)).toBeTruthy();
  });

  it("does not call a dispatcher that has never recorded a pass wedged", () => {
    // A freshly started daemon, or one from before passes were stamped, has null here — which is
    // an absence of evidence, not evidence of a stall.
    const fresh = stalled({
      machine: machine({ dispatcher_alive: true, dispatcher_pass_age_secs: null }),
    });
    render(<QueueVerdict data={fresh} loading={false} error={false} />);

    expect(screen.queryByText("The dispatcher is stuck")).toBeNull();
    expect(screen.getByText("online")).toBeTruthy();
  });

  it("separates an empty queue from a blocked one", () => {
    const empty = stalled({
      pending: 0,
      entries: [],
      decision: {
        at: 1_786_385_366,
        outcome: "nothing pending",
        dry_run: false,
        pending: 0,
        started: 0,
        deferred: [],
      },
    });
    render(<QueueVerdict data={empty} loading={false} error={false} />);

    expect(screen.getByText("Nothing is queued")).toBeTruthy();
    expect(screen.getByText("Nothing is being held back.")).toBeTruthy();
  });
});
