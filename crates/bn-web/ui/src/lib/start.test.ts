import { describe, expect, it } from "vitest";
import { defaultRepo, harnessAvailable, harnessReason, knownRepos, runningHarness } from "./start";
import type { FleetView, LiveAgent, RunView } from "../api/types";

function agent(over: Partial<LiveAgent>): LiveAgent {
  return {
    session_id: "s",
    harness: "claude",
    repo: "bn",
    branch: "main",
    target: "bn:main",
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

function fleet(over: Partial<FleetView>): FleetView {
  return { agents: [], active: 0, idle: 0, stuck: 0, total_tokens: 0, ...over };
}

function run(repo: string): RunView {
  return {
    repo,
    concurrency: 2,
    solo: false,
    stopped: false,
    drained: false,
    started_at: 0,
    updated_at: 0,
    counts: { queued: 0, in_flight: 0, merged: 0, failed: 0, total: 0 },
    queue: [],
    recovery: [],
  };
}

describe("knownRepos", () => {
  it("unions run + fleet repos, deduped and sorted", () => {
    const f = fleet({ agents: [agent({ repo: "zed" }), agent({ repo: "bn" })] });
    expect(knownRepos([run("bn"), run("aaa")], f)).toEqual(["aaa", "bn", "zed"]);
  });
});

describe("defaultRepo", () => {
  it("prefers the first run's repo", () => {
    expect(defaultRepo([run("bn"), run("zed")], undefined)).toBe("bn");
  });

  it("falls back to the first live-fleet repo when there are no runs", () => {
    const f = fleet({ agents: [agent({ repo: "zed" }), agent({ repo: "aaa" })] });
    expect(defaultRepo([], f)).toBe("aaa");
  });

  it("is empty when neither runs nor fleet carry a repo", () => {
    expect(defaultRepo([], fleet({}))).toBe("");
  });
});

describe("runningHarness", () => {
  it("returns the first live agent's dispatchable harness", () => {
    const f = fleet({ agents: [agent({ harness: "copilot" })] });
    expect(runningHarness(f)).toBe("copilot");
  });

  it("skips agents on non-dispatchable harnesses", () => {
    const f = fleet({ agents: [agent({ harness: "codex" }), agent({ harness: "claude" })] });
    expect(runningHarness(f)).toBe("claude");
  });

  it("is undefined with no live agents", () => {
    expect(runningHarness(fleet({}))).toBeUndefined();
  });
});

describe("harness availability", () => {
  it("treats an unknown harness (no fleet list) as available", () => {
    expect(harnessAvailable("claude", fleet({}))).toBe(true);
  });

  it("reflects a disabled harness from the fleet read", () => {
    const f = fleet({ harnesses: [{ name: "claude", available: false, reason: "org disabled" }] });
    expect(harnessAvailable("claude", f)).toBe(false);
    expect(harnessReason("claude", f)).toBe("org disabled");
  });

  it("reports an available harness with no reason", () => {
    const f = fleet({ harnesses: [{ name: "copilot", available: true }] });
    expect(harnessAvailable("copilot", f)).toBe(true);
    expect(harnessReason("copilot", f)).toBeUndefined();
  });
});
