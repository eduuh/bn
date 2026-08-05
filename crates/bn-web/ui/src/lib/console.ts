// The console's unified work model.
//
// The dashboard used to split one job across many screens: actions on the command center, running
// agents on the fleet, plan bodies on the agent page, decisions in a modal. But the operator's
// actual loop is a single supervision cycle —
//
//   signal → dispatch → agent works → agent asks → you decide → PR → land → done
//
// — and the only question that matters at any moment is *which open loop is blocked on me*. So the
// console folds every source into one ranked list of [`WorkItem`]s, grouped by **what you'd do
// about it** rather than by which API produced it. Pure and side-effect-free (`console.test.ts`)
// so the view stays a thin render.

import type { ActionItem, ActionKind } from "../api/types";
import type { FleetEntry } from "./fleet";

/** The four lanes of the console queue, in priority order. The lane answers "what do I do?" —
 * which is why a stuck agent and a pending plan review share a lane despite different origins. */
export type Lane = "blocked" | "running" | "backlog" | "done";

/** What the primary (Enter / big button) control does for an item. The old UI offered "Start agent"
 * on every row, including plan reviews — where the correct action is to *decide*, not to spawn a
 * second agent. Resolving the verb per item is the single biggest flow fix. */
export type Verb = "decide" | "watch" | "land" | "dispatch" | "open";

/** One entry in the console queue: either a live agent or an actionable signal, normalised so the
 * queue renders and ranks them the same way. */
export interface WorkItem {
  /** Stable selection key, also the `?sel=` URL value. */
  id: string;
  lane: Lane;
  verb: Verb;
  /** Short kind label for the row's tag ("Plan review", "Working", "Todo"). */
  kind: string;
  title: string;
  /** `repo/branch` when known — the row's second line. */
  where?: string;
  /** Extra context for the second line (source, state, pane label). */
  meta?: string;
  tone: "urgent" | "warn" | "ok" | "info" | "muted";
  /** Sort key within a lane — epoch seconds, most recent first. */
  updatedAt: number;
  /** Set for an agent row: the live session + tmux target it steers. */
  agent?: FleetEntry;
  /** Set for a signal row: the underlying action, carrying its dispatch payload + prompt. */
  action?: ActionItem;
}

/** Human label per action kind — shared by the row tag and the lane filters. */
export const KIND_LABEL: Record<ActionKind, string> = {
  attention: "Attention",
  "plan-review": "Plan review",
  todo: "Todo",
  blocker: "Blocker",
  "failing-cron": "Cron",
  "stale-pr": "Stale PR",
  goal: "Goal",
  issue: "Issue",
  suggestion: "Suggestion",
};

/** Human label per lane, for the queue's group headers. */
export const LANE_LABEL: Record<Lane, string> = {
  blocked: "Blocked on you",
  running: "Running",
  backlog: "Backlog",
  done: "Recently done",
};

/** A one-line explanation of what the lane means, shown under its header when empty. */
export const LANE_EMPTY: Record<Lane, string> = {
  blocked: "Nothing is waiting on a decision.",
  running: "No agents are working right now.",
  backlog: "No queued work — todos, goals and issues land here.",
  done: "Nothing finished recently.",
};

/** Signal tone for an action's severity. */
function severityTone(severity: string): WorkItem["tone"] {
  if (severity === "urgent") return "urgent";
  if (severity === "warn") return "warn";
  return "info";
}

/**
 * Which lane an action belongs in. The rule is "is a *decision* blocked on me right now?" —
 * a pending plan review or a failing cron is; a todo is work I could pick up, which is backlog.
 */
function actionLane(item: ActionItem): Lane {
  switch (item.type) {
    case "plan-review":
    case "blocker":
    case "failing-cron":
      return "blocked";
    case "attention":
    case "stale-pr":
      // Attention + stale PRs block you only when they're actually urgent; otherwise they queue.
      return item.severity === "urgent" ? "blocked" : "backlog";
    default:
      return "backlog";
  }
}

/** The primary verb for an action — what Enter does on its row. */
function actionVerb(item: ActionItem): Verb {
  if (item.type === "plan-review") return "decide";
  if (item.type === "stale-pr") return "land";
  return "dispatch";
}

/** An actionable signal → a queue item. */
function fromAction(item: ActionItem): WorkItem {
  const where = item.repo ? `${item.repo}${item.branch ? `/${item.branch}` : ""}` : undefined;
  return {
    id: `action:${item.id}`,
    lane: actionLane(item),
    verb: actionVerb(item),
    kind: KIND_LABEL[item.type] ?? item.type,
    title: item.title,
    where,
    meta: item.source,
    tone: severityTone(item.severity),
    updatedAt: item.created,
    action: item,
  };
}

/** A live agent → a queue item. An agent that needs you outranks everything it's running beside. */
function fromAgent(e: FleetEntry): WorkItem {
  return {
    id: `agent:${e.key}`,
    lane: e.needs ? "blocked" : "running",
    // A needy agent wants a decision (approve the plan, answer the prompt, relaunch it); a healthy
    // one just wants watching.
    verb: e.needs ? "decide" : "watch",
    // An agent is identified by *where it is*, and described by what it's doing — the reverse of a
    // signal, whose title is the thing itself. Putting `repo/branch` on the first line keeps the
    // running lane scannable ("which branches are live?") instead of a column of "Active".
    kind: e.role === "orchestrator" ? "Orchestrator" : (e.needsReason ?? e.stateLabel),
    title: `${e.repo}/${e.branch}`,
    meta: e.harness,
    tone: e.needs ? (e.tone === "red" ? "urgent" : "warn") : e.tone === "green" ? "ok" : "muted",
    updatedAt: e.updatedAt,
    agent: e,
  };
}

/** Rank within a lane: urgent first, then most-recently-updated. */
const TONE_RANK: Record<WorkItem["tone"], number> = {
  urgent: 0,
  warn: 1,
  ok: 2,
  info: 3,
  muted: 4,
};

/**
 * Build the full console queue from the live fleet and the action feed.
 *
 * Agents come first within a lane when tone ties, because a running process is more perishable
 * than a backlog row — if you only look at the top of the list, it should be the thing that
 * degrades if ignored.
 */
export function buildQueue(entries: FleetEntry[], actions: ActionItem[]): WorkItem[] {
  const items = [...entries.map(fromAgent), ...actions.map(fromAction)];
  const laneRank: Record<Lane, number> = { blocked: 0, running: 1, backlog: 2, done: 3 };
  items.sort(
    (a, b) =>
      laneRank[a.lane] - laneRank[b.lane] ||
      TONE_RANK[a.tone] - TONE_RANK[b.tone] ||
      Number(!!b.agent) - Number(!!a.agent) ||
      b.updatedAt - a.updatedAt,
  );
  return items;
}

/** Split a ranked queue into its lanes, preserving order. Empty lanes are kept so the queue can
 * render a reassuring "nothing is blocked on you" instead of silently omitting the lane. */
export function byLane(items: WorkItem[]): { lane: Lane; items: WorkItem[] }[] {
  const lanes: Lane[] = ["blocked", "running", "backlog"];
  return lanes.map((lane) => ({ lane, items: items.filter((i) => i.lane === lane) }));
}

/** Case-insensitive substring match across the fields a row actually shows. */
export function filterQueue(items: WorkItem[], q: string): WorkItem[] {
  const needle = q.trim().toLowerCase();
  if (!needle) return items;
  return items.filter((i) =>
    [i.title, i.where, i.meta, i.kind].some((f) => f?.toLowerCase().includes(needle)),
  );
}

/** The queue counts the shell badges read off. */
export function queueCounts(items: WorkItem[]): {
  blocked: number;
  running: number;
  backlog: number;
} {
  return {
    blocked: items.filter((i) => i.lane === "blocked").length,
    running: items.filter((i) => i.lane === "running").length,
    backlog: items.filter((i) => i.lane === "backlog").length,
  };
}
