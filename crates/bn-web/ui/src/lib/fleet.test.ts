import { describe, expect, it } from "vitest";
import { fleetEntries, needsYou, orchestrators, paneStateLabel } from "./fleet";
import type { FleetView, OrchestrateView, QueueItem, RunView } from "../api/types";

function item(over: Partial<QueueItem>): QueueItem {
  return {
    id: "1",
    item: "1",
    repo: "bn",
    status: "dispatched",
    attempts: 0,
    dispatched_at: 0,
    last_relaunch_at: 0,
    updated_at: 0,
    ...over,
  };
}

function run(over: Partial<RunView>): RunView {
  return {
    repo: "bn",
    concurrency: 2,
    solo: false,
    stopped: false,
    drained: false,
    started_at: 0,
    updated_at: 0,
    counts: { queued: 0, in_flight: 0, merged: 0, failed: 0, total: 0 },
    queue: [],
    recovery: [],
    ...over,
  };
}

function orch(queue: QueueItem[]): OrchestrateView {
  return { runs: [run({ queue })] };
}

describe("fleetEntries", () => {
  it("includes only dispatched (in-flight) items as fleet cards", () => {
    const view = orch([
      item({ id: "1", status: "dispatched", branch: "dispatch/a" }),
      item({ id: "2", status: "queued", branch: "dispatch/b" }),
      item({ id: "3", status: "merged", branch: "dispatch/c" }),
    ]);
    const entries = fleetEntries(view);
    expect(entries.map((e) => e.branch)).toEqual(["dispatch/a"]);
  });

  it("flags waiting-for-review and errored agents as needs-you, sorted first", () => {
    const view = orch([
      item({ id: "1", branch: "dispatch/working", last_state: "working", updated_at: 100 }),
      item({ id: "2", branch: "dispatch/review", waiting_for_review: true, review_id: "r1", updated_at: 50 }),
      item({ id: "3", branch: "dispatch/dead", last_state: "dead", updated_at: 60 }),
    ]);
    const entries = fleetEntries(view);
    const need = needsYou(entries);
    // Needs-you (review + dead) sort ahead of the plain working agent.
    expect(entries[0].needs).toBe(true);
    expect(need.map((e) => e.branch).sort()).toEqual(["dispatch/dead", "dispatch/review"]);
    const review = entries.find((e) => e.reviewId === "r1")!;
    expect(review.stateLabel).toBe("Waiting: plan review");
  });

  it("joins a live agent for tokens + session id", () => {
    const fleet: FleetView = {
      agents: [
        {
          session_id: "sess-x",
          harness: "claude",
          repo: "bn",
          branch: "dispatch-a",
          target: "bn:dispatch-a",
          last_activity_ts: 0,
          idle_secs: 0,
          health: "active",
          turns: 3,
          tool_calls: 0,
          input_tokens: 0,
          output_tokens: 4200,
          cache_read_tokens: 0,
        },
      ],
      active: 1,
      idle: 0,
      stuck: 0,
      total_tokens: 4200,
    };
    const view = orch([item({ id: "1", status: "dispatched", branch: "dispatch/a" })]);
    const entry = fleetEntries(view, fleet)[0];
    expect(entry.sessionId).toBe("sess-x");
    expect(entry.outputTokens).toBe(4200);
  });
});

describe("orchestrators", () => {
  function liveAgent(over: Partial<FleetView["agents"][number]>): FleetView["agents"][number] {
    return {
      session_id: "s",
      harness: "copilot",
      repo: "bn",
      branch: "b",
      target: "bn:b",
      last_activity_ts: 100,
      idle_secs: 30,
      health: "active",
      turns: 0,
      tool_calls: 0,
      input_tokens: 0,
      output_tokens: 0,
      cache_read_tokens: 0,
      ...over,
    };
  }

  it("pins an orchestrator-role live agent to the orchestrator tier and keeps a worker out", () => {
    const fleet: FleetView = {
      agents: [
        liveAgent({ session_id: "mon", branch: "wave", role: "orchestrator" }),
        liveAgent({ session_id: "ship", branch: "feat-x", role: "worker" }),
      ],
      active: 2,
      idle: 0,
      stuck: 0,
      total_tokens: 0,
    };
    // No orchestrate run, so both live agents flow through as non-orchestrated entries.
    const entries = fleetEntries({ runs: [] }, fleet);
    const orchs = orchestrators(entries);
    expect(orchs.map((e) => e.sessionId)).toEqual(["mon"]);
    expect(orchs[0].statusLabel).toBe("orchestrator");
    expect(orchs[0].stateLabel).toMatch(/last tick/);
    // The worker is not in the orchestrator tier.
    expect(orchs.some((e) => e.sessionId === "ship")).toBe(false);
  });

  it("treats a live agent with no role as a worker (pre-#500 payloads)", () => {
    const fleet: FleetView = {
      agents: [liveAgent({ session_id: "legacy", branch: "old" })],
      active: 1,
      idle: 0,
      stuck: 0,
      total_tokens: 0,
    };
    const entries = fleetEntries({ runs: [] }, fleet);
    expect(orchestrators(entries)).toHaveLength(0);
    expect(entries[0].role).toBe("worker");
  });

  it("renders a pending (starting) agent as a non-needs-you 'Starting…' entry, sorted first (#545)", () => {
    const fleet: FleetView = {
      agents: [
        liveAgent({ session_id: "old-worker", branch: "feat-x", health: "active", last_activity_ts: 100 }),
        // A just-dispatched agent with no transcript yet — synthesized server-side with the launch
        // time as its activity ts, so it sorts to the top as the newest work.
        liveAgent({ session_id: "pending", branch: "dispatch/web-545", health: "starting", last_activity_ts: 200 }),
      ],
      starting: 1,
      active: 1,
      idle: 0,
      stuck: 0,
      total_tokens: 0,
    };
    const entries = fleetEntries({ runs: [] }, fleet);
    const pending = entries.find((e) => e.sessionId === "pending")!;
    expect(pending.stateLabel).toBe("Starting…");
    expect(pending.needs).toBe(false);
    expect(needsYou(entries)).toHaveLength(0);
    // Newest (the pending launch) sorts first among the non-needs entries.
    expect(entries[0].sessionId).toBe("pending");
  });
});

describe("paneStateLabel", () => {
  it("maps machine states to human one-liners", () => {
    expect(paneStateLabel("working")).toBe("Working");
    expect(paneStateLabel("trust-menu")).toBe("Waiting: trust prompt");
    expect(paneStateLabel("command-approval")).toBe("Waiting: command approval");
    expect(paneStateLabel("dead")).toBe("Dead — needs relaunch");
    expect(paneStateLabel(null)).toBe("Dispatched");
  });
});
