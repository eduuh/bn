import { describe, it, expect } from "vitest";
import {
  applyQueuedOrder,
  moveId,
  optimisticItem,
  parseItems,
  patchRun,
  queuedItems,
  recount,
} from "./backlog";
import type { OrchestrateView, QueueItem, RunView } from "../api/types";

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

describe("parseItems", () => {
  it("splits lines, trims, drops empties, de-dupes order-preserving", () => {
    expect(parseItems("  #1 \n\n#2\n#1\n  \n#3 ")).toEqual(["#1", "#2", "#3"]);
  });
  it("is empty for blank input", () => {
    expect(parseItems("\n  \n")).toEqual([]);
  });
});

describe("queuedItems", () => {
  it("keeps only queued, in queue order", () => {
    const r = run({
      queue: [
        item({ id: "a", status: "queued" }),
        item({ id: "b", status: "dispatched" }),
        item({ id: "c", status: "queued" }),
      ],
    });
    expect(queuedItems(r).map((i) => i.id)).toEqual(["a", "c"]);
  });
});

describe("applyQueuedOrder", () => {
  it("reorders queued items in place, leaving non-queued slots untouched", () => {
    const queue = [
      item({ id: "a", status: "queued" }),
      item({ id: "d", status: "dispatched" }),
      item({ id: "b", status: "queued" }),
      item({ id: "c", status: "queued" }),
    ];
    const out = applyQueuedOrder(queue, ["c", "a", "b"]);
    // queued slots (0,2,3) now hold c,a,b; the dispatched item stays at slot 1.
    expect(out.map((i) => i.id)).toEqual(["c", "d", "a", "b"]);
  });
  it("appends queued ids not listed after the listed ones", () => {
    const queue = [
      item({ id: "a", status: "queued" }),
      item({ id: "b", status: "queued" }),
      item({ id: "c", status: "queued" }),
    ];
    expect(applyQueuedOrder(queue, ["c"]).map((i) => i.id)).toEqual(["c", "a", "b"]);
  });
});

describe("moveId", () => {
  it("moves an id to a new index", () => {
    expect(moveId(["a", "b", "c"], 0, 2)).toEqual(["b", "c", "a"]);
    expect(moveId(["a", "b", "c"], 2, 0)).toEqual(["c", "a", "b"]);
  });
  it("is a no-op for same-slot or out-of-range", () => {
    const ids = ["a", "b"];
    expect(moveId(ids, 1, 1)).toBe(ids);
    expect(moveId(ids, 0, 5)).toBe(ids);
  });
});

describe("patchRun", () => {
  it("replaces only the matching run", () => {
    const view: OrchestrateView = { runs: [run({ repo: "bn" }), run({ repo: "web" })] };
    const out = patchRun(view, "web", (r) => ({ ...r, concurrency: 9 }));
    expect(out.runs.find((r) => r.repo === "bn")!.concurrency).toBe(2);
    expect(out.runs.find((r) => r.repo === "web")!.concurrency).toBe(9);
    expect(out).not.toBe(view);
  });
});

describe("recount", () => {
  it("derives counts from the queue statuses", () => {
    const r = recount(
      run({
        queue: [
          item({ id: "a", status: "queued" }),
          item({ id: "b", status: "queued" }),
          item({ id: "c", status: "dispatched" }),
          item({ id: "d", status: "merged" }),
          item({ id: "e", status: "failed" }),
        ],
      }),
    );
    expect(r.counts).toEqual({ queued: 2, in_flight: 1, merged: 1, failed: 1, total: 5 });
  });
});

describe("optimisticItem", () => {
  it("is a queued item with a tmp id", () => {
    const it = optimisticItem("#42", 0, 1000);
    expect(it.status).toBe("queued");
    expect(it.item).toBe("#42");
    expect(it.id.startsWith("tmp:")).toBe(true);
  });
});
