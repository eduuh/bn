import { describe, it, expect } from "vitest";
import {
  agentForItem,
  columns,
  healthTone,
  jumpCommand,
  mergeSparkline,
  nonOrchestratedAgents,
  paneTone,
  prHref,
  prLabel,
  sanitizeBranch,
  throughput,
  tmuxTarget,
} from "./board";
import type { FleetView, LiveAgent, QueueItem, RunView } from "../api/types";

function item(over: Partial<QueueItem>): QueueItem {
  return {
    id: "x",
    item: "#x",
    status: "queued",
    attempts: 0,
    dispatched_at: 0,
    last_relaunch_at: 0,
    updated_at: 0,
    ...over,
  };
}

function agent(over: Partial<LiveAgent>): LiveAgent {
  return {
    session_id: "s",
    harness: "claude",
    repo: "bn",
    branch: "dispatch/352",
    target: "bn:dispatch-352",
    last_activity_ts: 0,
    idle_secs: 0,
    health: "active",
    turns: 0,
    tool_calls: 0,
    input_tokens: 0,
    output_tokens: 0,
    cache_read_tokens: 0,
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

describe("columns", () => {
  it("partitions the queue by status", () => {
    const r = run({
      queue: [
        item({ id: "1", status: "queued" }),
        item({ id: "2", status: "dispatched" }),
        item({ id: "3", status: "merged" }),
        item({ id: "4", status: "failed" }),
        item({ id: "5", status: "dispatched" }),
      ],
    });
    const c = columns(r);
    expect(c.queued.map((i) => i.id)).toEqual(["1"]);
    expect(c.inFlight.map((i) => i.id)).toEqual(["2", "5"]);
    expect(c.merged.map((i) => i.id)).toEqual(["3"]);
    expect(c.failed.map((i) => i.id)).toEqual(["4"]);
  });
});

describe("paneTone / healthTone", () => {
  it("maps pane states to the design colours", () => {
    expect(paneTone("working")).toBe("green");
    expect(paneTone("api-error")).toBe("red");
    expect(paneTone("dns-outage")).toBe("red");
    expect(paneTone("dead")).toBe("red");
    expect(paneTone("idle-with-wip")).toBe("amber");
    expect(paneTone("command-approval")).toBe("amber");
    expect(paneTone("question-menu")).toBe("amber");
    expect(paneTone("done")).toBe("grey");
    expect(paneTone("idle-clean")).toBe("grey");
    expect(paneTone(null)).toBe("neutral");
    expect(paneTone(undefined)).toBe("neutral");
  });
  it("maps agent health to tones", () => {
    expect(healthTone("active")).toBe("green");
    expect(healthTone("idle")).toBe("amber");
    expect(healthTone("stuck")).toBe("red");
  });
});

describe("tmuxTarget", () => {
  it("prefers #372 explicit tmux fields", () => {
    const it0 = item({ tmux_session: "bn", tmux_window: "dispatch-352", branch: "dispatch/352" });
    expect(tmuxTarget(it0, "bn")).toBe("bn:dispatch-352");
  });
  it("falls back to the joined live agent's target", () => {
    const it0 = item({ branch: "dispatch/352" });
    expect(tmuxTarget(it0, "bn", agent({ target: "bn:dispatch-352" }))).toBe("bn:dispatch-352");
  });
  it("reconstructs repo:sanitized-branch when nothing else is available", () => {
    const it0 = item({ branch: "feat/deep/nest" });
    expect(tmuxTarget(it0, "bn")).toBe("bn:feat-deep-nest");
  });
  it("is null with no branch and no agent", () => {
    expect(tmuxTarget(item({}), "bn")).toBeNull();
  });
  it("wraps into the jump command", () => {
    expect(jumpCommand("bn:dispatch-352")).toBe("tmux switch-client -t bn:dispatch-352");
  });
});

describe("agentForItem", () => {
  // Live fleet reports the dash (note-key) branch form; the queue keeps slashes.
  const fleet: FleetView = {
    agents: [agent({ branch: "dispatch-352" }), agent({ branch: "dispatch-353", session_id: "s2" })],
    active: 2,
    idle: 0,
    stuck: 0,
    total_tokens: 0,
  };
  it("joins by repo + branch across the slash/dash form mismatch", () => {
    expect(agentForItem(item({ branch: "dispatch/353" }), "bn", fleet)?.session_id).toBe("s2");
    expect(agentForItem(item({ branch: "dispatch/353" }), "BN", fleet)?.session_id).toBe("s2");
    expect(agentForItem(item({ branch: "dispatch-352" }), "bn", fleet)?.session_id).toBe("s");
  });
  it("returns undefined with no match / no branch / no fleet", () => {
    expect(agentForItem(item({ branch: "nope" }), "bn", fleet)).toBeUndefined();
    expect(agentForItem(item({}), "bn", fleet)).toBeUndefined();
    expect(agentForItem(item({ branch: "dispatch/352" }), "bn", undefined)).toBeUndefined();
  });
});

describe("throughput", () => {
  const now = 10 * 3600; // 10h since epoch
  it("is zero / null with no merges", () => {
    const tp = throughput(run({ counts: { queued: 3, in_flight: 0, merged: 0, failed: 0, total: 3 } }), now);
    expect(tp.mergedPerHour).toBe(0);
    expect(tp.etaHours).toBeNull();
  });
  it("computes merges/hr from run start and eta = queued / rate", () => {
    const r = run({
      started_at: now - 4 * 3600, // 4h ago
      counts: { queued: 4, in_flight: 0, merged: 2, failed: 0, total: 6 },
      queue: [
        item({ id: "a", status: "merged", updated_at: now - 3 * 3600 }),
        item({ id: "b", status: "merged", updated_at: now - 1 * 3600 }),
      ],
    });
    const tp = throughput(r, now);
    expect(tp.mergedPerHour).toBeCloseTo(0.5, 5); // 2 merges over 4h
    expect(tp.etaHours).toBeCloseTo(8, 5); // 4 queued / 0.5 per hr
  });
  it("eta is 0 when nothing is queued", () => {
    const r = run({
      started_at: now - 2 * 3600,
      counts: { queued: 0, in_flight: 0, merged: 1, failed: 0, total: 1 },
      queue: [item({ id: "a", status: "merged", updated_at: now - 1 * 3600 })],
    });
    expect(throughput(r, now).etaHours).toBe(0);
  });
});

describe("mergeSparkline", () => {
  it("buckets merges into the trailing hours aligned to now", () => {
    const now = 20 * 3600 + 1800; // 20:30 past epoch
    const r = run({
      queue: [
        item({ id: "a", status: "merged", updated_at: 20 * 3600 }), // this hour
        item({ id: "b", status: "merged", updated_at: 20 * 3600 + 60 }), // this hour
        item({ id: "c", status: "merged", updated_at: 19 * 3600 }), // last hour
        item({ id: "d", status: "queued", updated_at: 20 * 3600 }), // ignored
      ],
    });
    const { xs, ys } = mergeSparkline(r, now, 12);
    expect(xs).toHaveLength(12);
    expect(ys).toHaveLength(12);
    expect(ys[11]).toBe(2); // current hour bucket (last)
    expect(ys[10]).toBe(1); // previous hour
    expect(ys.reduce((s, v) => s + v, 0)).toBe(3);
  });
});

describe("nonOrchestratedAgents", () => {
  it("excludes agents whose repo/branch is in a run queue (slash/dash normalized)", () => {
    const fleet: FleetView = {
      agents: [
        agent({ session_id: "tracked", repo: "bn", branch: "dispatch-352" }),
        agent({ session_id: "free", repo: "bn", branch: "feature-x" }),
        agent({ session_id: "otherrepo", repo: "web", branch: "dispatch-352" }),
      ],
      active: 3,
      idle: 0,
      stuck: 0,
      total_tokens: 0,
    };
    // Queue branch keeps the slash; the tracked agent reports the dash form.
    const runs = [run({ repo: "bn", queue: [item({ branch: "dispatch/352", status: "dispatched" })] })];
    const rail = nonOrchestratedAgents(fleet, runs).map((a) => a.session_id);
    expect(rail).toEqual(["free", "otherrepo"]);
  });
  it("returns [] when fleet is undefined", () => {
    expect(nonOrchestratedAgents(undefined, [])).toEqual([]);
  });
});

describe("pr + branch helpers", () => {
  it("sanitizes branch slashes for tmux windows", () => {
    expect(sanitizeBranch("dispatch/352")).toBe("dispatch-352");
    expect(sanitizeBranch("a/b/c")).toBe("a-b-c");
  });
  it("links only real URLs and labels PRs", () => {
    expect(prHref("https://github.com/eduuh/bn/pull/390")).toBe(
      "https://github.com/eduuh/bn/pull/390",
    );
    expect(prHref("390")).toBeNull();
    expect(prHref(null)).toBeNull();
    expect(prLabel("https://github.com/eduuh/bn/pull/390")).toBe("#390");
    expect(prLabel("#390")).toBe("#390");
    expect(prLabel("390")).toBe("#390");
  });
});
