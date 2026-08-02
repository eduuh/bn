import { describe, expect, it } from "vitest";
import { buildQueue, byLane, filterQueue, queueCounts } from "./console";
import type { FleetEntry } from "./fleet";
import type { ActionItem, ActionKind } from "../api/types";

function agent(over: Partial<FleetEntry> = {}): FleetEntry {
  return {
    key: "k1",
    repo: "bn",
    branch: "main",
    target: "bn:main",
    role: "worker",
    stateLabel: "Working",
    statusLabel: "working",
    tone: "green",
    updatedAt: 100,
    needs: false,
    ...over,
  };
}

function action(type: ActionKind, over: Partial<ActionItem> = {}): ActionItem {
  return {
    id: `${type}-1`,
    type,
    title: `a ${type}`,
    source: "test",
    severity: "info",
    payload: {},
    suggested_prompt: "do it",
    default_urgency: "queued",
    created: 50,
    stale: false,
    ...over,
  };
}

describe("buildQueue lanes", () => {
  it("puts a needy agent in `blocked` and a healthy one in `running`", () => {
    const q = buildQueue([agent({ key: "a", needs: true, needsReason: "No progress" }), agent({ key: "b" })], []);
    expect(q.find((i) => i.id === "agent:a")?.lane).toBe("blocked");
    expect(q.find((i) => i.id === "agent:b")?.lane).toBe("running");
  });

  it("treats plan-reviews, blockers and failing crons as blocking regardless of severity", () => {
    const q = buildQueue([], [action("plan-review"), action("blocker"), action("failing-cron")]);
    expect(q.every((i) => i.lane === "blocked")).toBe(true);
  });

  it("routes attention to `blocked` only when urgent", () => {
    const q = buildQueue(
      [],
      [
        action("attention", { id: "hot", severity: "urgent" }),
        action("attention", { id: "cool", severity: "warn" }),
      ],
    );
    expect(q.find((i) => i.id === "action:hot")?.lane).toBe("blocked");
    expect(q.find((i) => i.id === "action:cool")?.lane).toBe("backlog");
  });

  it("keeps todos and goals in the backlog", () => {
    const q = buildQueue([], [action("todo"), action("goal")]);
    expect(q.every((i) => i.lane === "backlog")).toBe(true);
  });
});

describe("buildQueue verbs", () => {
  it("asks you to decide a plan review, not to dispatch another agent", () => {
    const [item] = buildQueue([], [action("plan-review")]);
    expect(item.verb).toBe("decide");
  });

  it("offers `land` for a stale PR and `dispatch` for ordinary work", () => {
    const q = buildQueue([], [action("stale-pr"), action("todo")]);
    expect(q.find((i) => i.kind === "Stale PR")?.verb).toBe("land");
    expect(q.find((i) => i.kind === "Todo")?.verb).toBe("dispatch");
  });

  it("watches a healthy agent and decides on a stuck one", () => {
    const q = buildQueue([agent({ key: "ok" }), agent({ key: "bad", needs: true })], []);
    expect(q.find((i) => i.id === "agent:ok")?.verb).toBe("watch");
    expect(q.find((i) => i.id === "agent:bad")?.verb).toBe("decide");
  });
});

describe("buildQueue ranking", () => {
  it("ranks every blocked item above every running one, and running above backlog", () => {
    const q = buildQueue(
      [agent({ key: "run" }), agent({ key: "stuck", needs: true, tone: "red" })],
      [action("todo")],
    );
    expect(q.map((i) => i.lane)).toEqual(["blocked", "running", "backlog"]);
  });

  it("breaks a lane tie by tone before recency", () => {
    const q = buildQueue(
      [],
      [
        action("blocker", { id: "old-urgent", severity: "urgent", created: 1 }),
        action("failing-cron", { id: "new-warn", severity: "warn", created: 999 }),
      ],
    );
    expect(q[0].id).toBe("action:old-urgent");
  });

  it("prefers a live agent over a signal when lane and tone tie", () => {
    const q = buildQueue(
      [agent({ key: "live", needs: true, tone: "red", updatedAt: 1 })],
      [action("blocker", { severity: "urgent", created: 999 })],
    );
    expect(q[0].id).toBe("agent:live");
  });
});

describe("agent rows", () => {
  it("identifies an agent by repo/branch and describes it by state", () => {
    const [item] = buildQueue([agent({ needs: true, needsReason: "No progress" })], []);
    expect(item.title).toBe("bn/main");
    expect(item.kind).toBe("No progress");
  });

  it("labels an orchestrator distinctly", () => {
    const [item] = buildQueue([agent({ role: "orchestrator" })], []);
    expect(item.kind).toBe("Orchestrator");
  });
});

describe("byLane", () => {
  it("returns all three lanes even when empty, so the queue can reassure rather than omit", () => {
    const lanes = byLane(buildQueue([], []));
    expect(lanes.map((l) => l.lane)).toEqual(["blocked", "running", "backlog"]);
    expect(lanes.every((l) => l.items.length === 0)).toBe(true);
  });
});

describe("filterQueue", () => {
  it("matches case-insensitively across title, location and kind", () => {
    const q = buildQueue([agent({ repo: "dotfiles" })], [action("todo", { title: "Wire the chart" })]);
    expect(filterQueue(q, "DOTFILES")).toHaveLength(1);
    expect(filterQueue(q, "chart")).toHaveLength(1);
    expect(filterQueue(q, "todo")).toHaveLength(1);
    expect(filterQueue(q, "nothing")).toHaveLength(0);
  });

  it("returns everything for a blank or whitespace query", () => {
    const q = buildQueue([agent()], [action("todo")]);
    expect(filterQueue(q, "   ")).toHaveLength(2);
  });
});

describe("queueCounts", () => {
  it("counts each lane", () => {
    const q = buildQueue(
      [agent({ key: "a", needs: true }), agent({ key: "b" })],
      [action("todo"), action("goal")],
    );
    expect(queueCounts(q)).toEqual({ blocked: 1, running: 1, backlog: 2 });
  });
});
