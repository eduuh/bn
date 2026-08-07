/**
 * Queue viewmodels (#queue) — the shapes `bn_core::queue` serializes.
 *
 * A separate file from `types.ts`, which is already the largest in the UI: the queue's surface
 * grows with every producer the epic absorbs.
 */
import type { TaskRow } from "./types";

/**
 * The day timeline (#queue) — the viewmodel `bn_core::queue::timeline::Timeline` serializes.
 *
 * Times are local epoch seconds. `now` comes from the server rather than the browser so the
 * current-time rule is drawn from the same clock that placed the marks.
 */
export interface TimelineMark {
  key: string;
  label: string;
  /** Kebab-case BacklogKind — the colour key. */
  kind: string;
  /** `coding` | `research`. */
  lane: string;
  priority: number;
  at: number;
  end: number;
  /** True when `at` is a projection rather than a fact; drawn hollow and dashed. */
  projected: boolean;
  note: string;
  node?: string;
  blocked?: boolean;
}

export interface TimelineBand {
  id: "running" | "scheduled" | "projected" | "done";
  label: string;
  marks: TimelineMark[];
}

export interface TimelineHourLoad {
  hour: number;
  running: number;
  scheduled: number;
  projected: number;
}

/**
 * A pause scope, as `PauseScope` serializes it: internally tagged on `scope`, with the axis value
 * beside it. A discriminated union rather than an index signature, so narrowing the tag actually
 * tells the compiler which field carries the value.
 */
export type QueuePauseScope =
  | { scope: "all" }
  | { scope: "kind"; kind: string }
  | { scope: "lane"; lane: string }
  | { scope: "node"; node_id: string }
  | { scope: "repo"; repo: string };

export interface QueuePause {
  scope: QueuePauseScope;
  since: number;
  reason?: string;
  until?: number;
}

export interface QueueTimelineView {
  date: string;
  start: number;
  end: number;
  now: number;
  pauses: QueuePause[];
  bands: TimelineBand[];
  load: TimelineHourLoad[];
  /** Pending entries that do not fit the day at projected throughput — counted, never drawn. */
  beyond: number;
}

/** One entry the dispatcher considered and did not start, with the reason it recorded. */
export interface QueueDeferredRow {
  repo: string;
  id: string;
  kind: string;
  /** `paused` | `capacity` | `kind-cap` | `conflict` | `cant-merge` | `no-capable-node`. */
  reason: string;
  detail?: string;
  waiting_since: number;
}

/** A node and what it can actually run. */
export interface QueueNodeCapacity {
  mem_total_mb: number | null;
  mem_available_mb: number | null;
  swap_total_mb: number | null;
  swap_free_mb: number | null;
  loadavg1: number | null;
  nproc: number;
  disk_avail_mb: number | null;
}

export interface QueueNodeRow {
  node_id: string;
  target: string;
  status: string;
  reported_status: string;
  stale: boolean;
  last_heartbeat: number;
  heartbeat_age_secs: number | null;
  slots: number;
  busy: number;
  free: number;
  skills: string[];
  harnesses: string[];
  labels: string[];
  owned_repos: string[];
  ram_total_mb: number;
  ram_reserved_mb: number;
  capacity: QueueNodeCapacity | null;
}

/** The persisted explanation of the last tick — what `bn queue why` prints. */
export interface QueueDecision {
  at: number;
  outcome: string;
  bind?: string;
  reason?: string;
  dry_run: boolean;
  pending: number;
  started: number;
  deferred: QueueDeferredRow[];
}

/** One queued entry, as `entries` carries it. */
export interface QueueEntry {
  id: string;
  repo: string;
  item: string;
  kind: string;
  priority: number;
  status: string;
  added_at: number;
  updated_at: number;
  exec?: { run?: string; job?: string; skill?: string; cmd?: string };
}

/** A lane's ceiling and how much of it is in use. */
export interface QueueLane {
  lane: string;
  cap: number;
  used: number;
}

/** The state of the machine itself — what answers "why is nothing running". */
export interface QueueMachine {
  /** False means producers are launching directly and every count below is beside the point. */
  dispatcher_alive: boolean;
  captured: string[];
  lanes: QueueLane[];
  /** How the lane caps were derived, so a surprising number is answerable. */
  lanes_why: string;
  builds: { cap: number; held: Array<{ id: string; label: string; repo: string; since: number }> };
  windows: Array<{ name: string; boost: number; matches: string[] }>;
}

/** `GET /api/queue` .data — the machine-wide queue. */
export interface QueueView {
  now: number;
  paused: boolean;
  fully_paused: boolean;
  pauses: QueuePause[];
  pending: number;
  kinds: Array<{ kind: string; lane: string; pending: number }>;
  entries: QueueEntry[];
  machine?: QueueMachine;
  nodes: QueueNodeRow[];
  decision?: QueueDecision | null;
  /** The task reservoir the backlog draws from. `null` when the task store could not be read — an
   *  unreadable store must not render as an empty one. */
  reservoir?: QueueReservoir | null;
}

/** Open tasks by distance from executable — the half of the queue that lives in the task store.
 *
 * Since #1001 work is minted as a task and only becomes a backlog entry once it has passed the
 * SMART gate, so these counts are what "waiting" actually means; `QueueView.pending` counts only
 * the far end of the same pipe. */
export interface QueueReservoir {
  /** Open, no `smart:` verdict yet — unjudged, so nothing may execute it. */
  needs_scoping: number;
  /** `smart:ok` — gated and eligible to be dispatched. */
  ready: number;
  /** `smart:fail` — the improve agent is still retrying. */
  failed: number;
  /** `smart:stuck` — out of retries, waiting on a person. */
  stuck: number;
  total: number;
}

/** One line of dispatcher history from `GET /api/queue/events`. */
export interface QueueEventRow {
  at: number;
  kind: string;
  repo?: string;
  id?: string;
  summary: string;
  /** How it reads, carried structurally so the UI colours without parsing the summary. */
  tone: "info" | "start" | "bad";
  /** Complete append-only event payload; never reconstructed from the summary. */
  fields: Record<string, unknown>;
}

export interface QueueRunTransition {
  at: number;
  kind: string;
  summary: string;
  tone: QueueEventRow["tone"];
  fields: Record<string, unknown>;
}

/** A dispatch paired with its terminal event, or one standalone operational outcome. */
export interface QueueRunRow {
  key: string;
  started_at: number;
  ended_at: number | null;
  duration_secs: number;
  repo: string;
  id: string;
  work: string;
  work_kind: string;
  lane: string;
  exec: string;
  node: string;
  harness: string;
  priority: number | null;
  rationale: string;
  task: string;
  entry_status: string;
  outcome: string;
  summary: string;
  tone: QueueEventRow["tone"];
  transitions: QueueRunTransition[];
}

/** `GET /api/queue/events` .data — the dispatcher's recent history. */
export interface QueueEventsView {
  now: number;
  since: number;
  /** Every filterable kind, from the server, so the chips cannot drift from what it accepts. */
  kinds: string[];
  nodes: string[];
  unknown_kind?: string | null;
  events: QueueEventRow[];
  runs: QueueRunRow[];
}

/** `GET /api/queue/entries` .data — every backlog entry, filterable. */
export interface QueueEntriesView {
  entries: QueueEntry[];
  /** How many matched before the limit, so a truncated list can say so. */
  total: number;
  /** Per-status totals over the *unfiltered* set — what is behind each chip. */
  counts: Record<string, number>;
  /** Lines that could not be parsed. Loud, because a queue losing work silently is the worst case. */
  unreadable?: number;
}

/** `GET /api/tasks?status=all` — the durable task file, including terminal rows. */
export interface TaskLedgerView {
  today: string;
  all: true;
  status: "all";
  query: string | null;
  repo: string | null;
  smart: string | null;
  stage: string | null;
  day: string | null;
  state: string | null;
  offset: number;
  limit: number;
  total: number;
  count: number;
  counts: Record<string, number>;
  tasks: TaskRow[];
}
