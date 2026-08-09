// REST client + react-query descriptors. Every read unwraps the `Envelope`, so components and the
// SSE hook both hold the bare viewmodel in the cache under a stable key. The `useBnStream` hook
// patches these same keys live via `setQueryData`; the `refetchInterval` is a belt-and-suspenders
// fallback if a stream frame is ever missed.

import { keepPreviousData } from "@tanstack/react-query";

import type {
  QueueEntriesView,
  QueueEventsView,
  TaskLedgerView,
  QueueTimelineView,
  QueueView,
} from "./queue-types";
import type {
  TasksView,
  TaskDetailView,
  IssuesView,
  PipelineBoardView,
  PipelineStagesView,
  DoneTasksView,
  FleetView,
  MachineView,
  NodesView,
  OrchestrateView,
  RecoveryView,
  CostView,
  Envelope,
  AgentDetailView,
  ActivityView,
  NotesView,
  NoteDetail,
  HandoffView,
  AttnView,
  ActionsView,
  CompletedView,
  ReviewsView,
  LearningsView,
  CronView,
  CatalogView,
  TodayAgentView,
  FindingsView,
  SuggestionsView,
  SkillsView,
  SkillDetail,
  GoalsView,
  WrapView,
  ReposView,
  RepoScriptView,
  RepoIssuesView,
  SearchView,
  MetricsView,
  MetricsTrendView,
  InsightsView,
  WeeklyView,
  BragView,
  HealthView,
} from "./types";
import type { NoteFileView, NoteFilesView, NotePlanView } from "./plan-types";

/** Fetch a bn read endpoint and return its envelope `data`, throwing on a transport/api failure. */
/** A failed read. Carries the HTTP status so a caller can tell "no such thing" (404) from "the
 * server broke" (500) — a distinction the message string alone forced every caller to parse.
 * Still an `Error`, so existing `(err as Error).message` handling is unaffected. */
export class FetchError extends Error {
  readonly status: number;
  readonly code?: string;
  constructor(message: string, status: number, code?: string) {
    super(message);
    this.name = "FetchError";
    this.status = status;
    this.code = code;
  }
}

export async function fetchEnvelope<T>(url: string): Promise<T> {
  const res = await fetch(url);
  if (!res.ok) {
    // The body is a bn envelope even on a non-2xx, so prefer its message over the bare status.
    const env = (await res.json().catch(() => null)) as Envelope<T> | null;
    throw new FetchError(
      env?.error?.message ?? `${url} → HTTP ${res.status}`,
      res.status,
      env?.error?.code ?? undefined,
    );
  }
  const env = (await res.json()) as Envelope<T>;
  if (!env.ok || env.data == null) {
    throw new FetchError(
      env.error?.message ?? `${url} returned a failure envelope`,
      res.status,
      env.error?.code ?? undefined,
    );
  }
  return env.data;
}

/** The react-query cache keys the SSE stream and the components share. */
export const queryKeys = {
  fleet: ["fleet"] as const,
  /** The server's own vitals — version, identity, bind, uptime (the settings page). */
  health: ["health"] as const,
  /** Machine-health snapshot (#525). */
  machine: ["machine"] as const,
  /** The fleet's machines as a list (#966 nodes view) — local today, remote-ready. */
  nodes: ["nodes"] as const,
  orchestrate: ["orchestrate"] as const,
  recovery: ["recovery"] as const,
  /** Cost is parameterized by window; the breakdown dimension is picked client-side (the view
   * carries every dimension), so only the window is part of the key / the refetch. */
  cost: (windowDays: number) => ["cost", windowDays] as const,
  /** One agent's detail header, keyed by session id. */
  agent: (id: string) => ["agent", id] as const,
  /** One session's activity timeline, keyed by session id. */
  activity: (sessionId: string) => ["activity", sessionId] as const,
  /** Every activity feed, as a prefix — for writes that know a `repo:branch` target but not which
   * session is on screen (steer, harness switch). react-query matches keys by prefix. */
  activityAll: ["activity"] as const,
  /** The branch-note rollup list. */
  notes: ["notes"] as const,
  /** One full note, keyed by repo + note-key branch. */
  note: (repo: string, branch: string) => ["note", repo, branch] as const,
  /** One branch note's plan artifact status. */
  notePlan: (repo: string, branch: string) => ["note", repo, branch, "plan"] as const,
  /** One branch note's bn-owned artifact directory listing. */
  noteFiles: (repo: string, branch: string, path: string) =>
    ["note", repo, branch, "files", path] as const,
  /** One branch note's bn-owned artifact file content. */
  noteFile: (repo: string, branch: string, path: string) =>
    ["note", repo, branch, "file", path] as const,
  /** One branch's handoff resume packet, keyed by repo + note-key branch (#509). */
  handoff: (repo: string, branch: string) => ["handoff", repo, branch] as const,
  /** The task store, keyed by the filter in play (#996). */
  tasks: (scope: string) => ["tasks", scope] as const,
  /** One task, by id — the `/tasks/:id` detail page. */
  task: (id: string) => ["task", id] as const,
  /** One task's attachments. Separate from the detail key: a directory scan should not run on
   * every poll of a view that refreshes on the fleet's cadence. */
  taskAttachments: (id: string) => ["task", id, "attachments"] as const,
  /** Every repo's open issues, with per-repo freshness (#999). */
  issues: ["issues"] as const,
  /** The nine-stage pipeline board (#746 / #1005). */
  pipeline: ["pipeline"] as const,
  /** The pipeline joined to its tasks and their agents — one key per drill-down. */
  pipelineStages: ["pipeline", "stages"] as const,
  /** A day's finished record: completions and cancellations, kept apart. */
  doneTasks: (day: string) => ["tasks", "done", day] as const,
  /** Open attention items (drives the plan-review panel). */
  attn: ["attn"] as const,
  /** The unified action-center list (#522) — every actionable signal, ranked. */
  actions: ["actions"] as const,
  /** The completed history (#561) — every finished task, most-recent-first. */
  completed: ["completed"] as const,
  /** Plan-reviews (#349) — the gate feed for the panel + board list. */
  reviews: ["reviews"] as const,
  /** The cron registry + last-run health (#483). */
  cron: ["cron"] as const,
  /** One cron source's stored findings history (#576), keyed by source/job name. */
  findings: (source: string) => ["findings", source] as const,
  /** Today's agent's suggested tasks (#577), keyed by the status filter (`` = all). */
  suggestions: (status: string) => ["suggestions", status] as const,
  /** The triggerable-agent catalog (#557) — crons + native jobs + runnable Markdown agents. */
  catalog: ["agents-catalog"] as const,
  /** Today's scheduled crons (#575) — the crons whose schedule fires today. */
  todayCrons: ["today-crons"] as const,
  /** The machine-wide work queue (#queue). */
  queue: ["queue"] as const,
  /** The dispatcher's recent history (#queue), keyed by its filters. */
  queueEvents: (since: string, kind: string, repo: string) =>
    ["queue", "events", since, kind, repo] as const,
  /** Every backlog entry (#queue), keyed by its filters. */
  queueEntries: (status: string, repo: string, kind: string, klass: string) =>
    ["queue", "entries", status, repo, kind, klass] as const,
  /** The day timeline (#queue), keyed by the date and class it renders. */
  queueTimeline: (date?: string, klass?: string) =>
    ["queue-timeline", date ?? "today", klass ?? ""] as const,
  /** Today's scheduled Markdown agents (#575). */
  todayAgents: ["today-agents"] as const,
  /** Today's one *hosted* agent — the live session the Today page streams and steers. */
  todayAgent: ["today-agent"] as const,
  /** Every skill across the tiers (#483). */
  skills: ["skills"] as const,
  /** One skill's editable source, keyed by name (#483). */
  skill: (name: string) => ["skill", name] as const,
  /** Today's daily goals + the current quarter (the Today view's "aimed" / #483 goals-surface). */
  goals: ["goals"] as const,
  /** Today's "achieved" digest — merged PRs, commits, todos, carry-forward (`bn wrap`). */
  wrap: ["wrap"] as const,
  /** The multi-repo navigation rollup (#495). */
  repos: ["repos"] as const,
  /** One repo's open GitHub issues (#556), keyed by repo name. */
  repoIssues: (repo: string) => ["repo-issues", repo] as const,
  /** One repo's effective/setup script source. */
  repoScript: (repo: string, name: string) => ["repo-script", repo, name] as const,
  /** The command-palette search result set (#565), keyed by the trimmed query. */
  search: (q: string) => ["search", q] as const,
  /** The learnings feed, keyed by its filter. */
  learnings: (filter: LearningsFilter) =>
    [
      "learnings",
      filter.since ?? null,
      filter.all ?? false,
      filter.repo ?? "",
      filter.skill ?? "",
      filter.agent ?? "",
    ] as const,
  /** The metrics scorecard (#507), keyed by trailing window. */
  metrics: (windowDays: number) => ["metrics", windowDays] as const,
  /** The metrics snapshot history behind the trend chart (#507). */
  metricsTrend: ["metrics-trend"] as const,
  /** The weekly auto-retro digest (#507). */
  insights: ["insights"] as const,
  /** The weekly 1-1 review digest (#507). */
  weekly: ["weekly"] as const,
  /** The accomplishments / impact packet (#507). */
  brag: ["brag"] as const,
};

export function fleetQuery() {
  return {
    queryKey: queryKeys.fleet,
    queryFn: () => fetchEnvelope<FleetView>("/api/fleet"),
    refetchInterval: 15_000,
  };
}

/** The server's own vitals. Nothing here moves except the uptime, so it is polled slowly — it is
 * a settings-page readout, not a monitor. */
export function healthQuery() {
  return {
    queryKey: queryKeys.health,
    queryFn: () => fetchEnvelope<HealthView>("/api/health"),
    refetchInterval: 60_000,
  };
}

/** Machine health (#525) — polled a touch faster than the fleet so a near-saturation warning
 * lands promptly (the whole point is seeing it *before* the machine melts). */
export function machineQuery() {
  return {
    queryKey: queryKeys.machine,
    queryFn: () => fetchEnvelope<MachineView>("/api/machine"),
    refetchInterval: 10_000,
  };
}

/** The fleet's machines (nodes view) — same cadence as machine health, and for the same reason:
 * an unreachable node should surface promptly. */
export function nodesQuery() {
  return {
    queryKey: queryKeys.nodes,
    queryFn: () => fetchEnvelope<NodesView>("/api/nodes"),
    refetchInterval: 10_000,
  };
}

export function orchestrateQuery() {
  return {
    queryKey: queryKeys.orchestrate,
    queryFn: () => fetchEnvelope<OrchestrateView>("/api/orchestrate"),
    refetchInterval: 15_000,
  };
}

export function recoveryQuery() {
  return {
    queryKey: queryKeys.recovery,
    queryFn: () => fetchEnvelope<RecoveryView>("/api/orchestrate/recovery?limit=50"),
  };
}

/** The `GET /api/cost` fold for a trailing `windowDays` window (#391). The `fleet` frame drives the
 * page's live section separately, so the fold itself only needs a slow fallback poll. */
export function costQuery(windowDays: number) {
  return {
    queryKey: queryKeys.cost(windowDays),
    queryFn: () => fetchEnvelope<CostView>(`/api/cost?window=${windowDays}`),
    refetchInterval: 60_000,
  };
}

/** One agent's detail header (fleet entry + latest recorded session + last request). The SSE `fleet`
 * frame keeps the underlying fleet fresh, so this only needs a slow fallback poll. */
export function agentDetailQuery(id: string) {
  return {
    queryKey: queryKeys.agent(id),
    queryFn: () => fetchEnvelope<AgentDetailView>(`/api/agents/${encodeURIComponent(id)}`),
    refetchInterval: 15_000,
  };
}

/** The agent's session timeline, polled every 4s while the tab is visible (a hidden tab returns
 * `false`, pausing the poll). Each tick re-reads and folds a transcript that can be a megabyte, and
 * prose doesn't change as fast as a spinner does, so this is deliberately unhurried.
 * `keepPreviousData` holds the last feed on screen while a refetch is in flight, so the view never
 * blanks between ticks. */
export function activityQuery(sessionId: string, limit = 40) {
  return {
    queryKey: queryKeys.activity(sessionId),
    queryFn: () =>
      fetchEnvelope<ActivityView>(
        `/api/agents/${encodeURIComponent(sessionId)}/activity?limit=${limit}`,
      ),
    refetchInterval: () => (document.visibilityState === "visible" ? 4000 : (false as const)),
    placeholderData: keepPreviousData,
    retry: 0,
    staleTime: 0,
  };
}

/** The branch-note rollup list (left rail of the Notes page). */
export function notesQuery() {
  return {
    queryKey: queryKeys.notes,
    queryFn: () => fetchEnvelope<NotesView>("/api/notes"),
    refetchInterval: 30_000,
  };
}

/** One full note (the Notes page's right pane). `branch` is the note-key form (slashes→dashes). */
export function noteDetailQuery(repo: string, branch: string) {
  return {
    queryKey: queryKeys.note(repo, branch),
    queryFn: () =>
      fetchEnvelope<NoteDetail>(
        `/api/notes/${encodeURIComponent(repo)}/${encodeURIComponent(branch)}`,
      ),
  };
}

export function notePlanQuery(repo: string, branch: string) {
  return {
    queryKey: queryKeys.notePlan(repo, branch),
    queryFn: () =>
      fetchEnvelope<NotePlanView>(
        `/api/notes/${encodeURIComponent(repo)}/${encodeURIComponent(branch)}/plan`,
      ),
  };
}

export function noteFilesQuery(repo: string, branch: string, path = "") {
  return {
    queryKey: queryKeys.noteFiles(repo, branch, path),
    queryFn: () =>
      fetchEnvelope<NoteFilesView>(
        `/api/notes/${encodeURIComponent(repo)}/${encodeURIComponent(branch)}/files?path=${encodeURIComponent(path)}`,
      ),
  };
}

export function noteFileQuery(repo: string, branch: string, path: string) {
  return {
    queryKey: queryKeys.noteFile(repo, branch, path),
    queryFn: () =>
      fetchEnvelope<NoteFileView>(
        `/api/notes/${encodeURIComponent(repo)}/${encodeURIComponent(branch)}/file?path=${encodeURIComponent(path)}`,
      ),
    enabled: !!path,
  };
}

/** One branch's handoff resume packet (#509) — the written `handoff.json`, or one composed live
 * from the note when none exists (mirroring `bn handoff read`). `enabled` defers the fetch until a
 * branch is selected. `staleTime: 0` so a Save (POST) re-read always reflects the just-written
 * packet. */
export function handoffQuery(repo: string | null, branch: string | null) {
  return {
    queryKey: queryKeys.handoff(repo ?? "", branch ?? ""),
    queryFn: () =>
      fetchEnvelope<HandoffView>(
        `/api/handoff?repo=${encodeURIComponent(repo ?? "")}&branch=${encodeURIComponent(branch ?? "")}`,
      ),
    enabled: !!repo && !!branch,
    staleTime: 0,
  };
}

/** Open attention items — drives the agent-detail plan-review panel and the board's review list. */
export function attnQuery() {
  return {
    queryKey: queryKeys.attn,
    queryFn: () => fetchEnvelope<AttnView>("/api/attn"),
    refetchInterval: 20_000,
    placeholderData: keepPrevious<AttnView>,
    ...TODAY_CACHE.work,
  };
}

/** The unified action-center list (#522) — every actionable signal bn tracks, ranked urgent-first,
 * each with an editable suggested prompt + default urgency. The command center's heart reads here.
 * Aggregates several stores server-side, so a moderate fallback poll keeps it fresh. */
export function actionsQuery() {
  return {
    queryKey: queryKeys.actions,
    queryFn: () => fetchEnvelope<ActionsView>("/api/actions"),
    refetchInterval: 20_000,
  };
}

/** The Completed tab's history (#561) — finished todos, goals, attention, and merged work, each
 * with local + remote note jumps. Changes slowly (things complete over minutes/hours), so a lazy
 * fallback poll is plenty. */
export function completedQuery() {
  return {
    queryKey: queryKeys.completed,
    queryFn: () => fetchEnvelope<CompletedView>("/api/completed"),
    refetchInterval: 60_000,
  };
}

/** Plan-reviews (#349) — the gate feed. Drives the agent-detail panel (the pending review for that
 * agent) and the board's pending-reviews list. Polls a little faster since it's the human's queue. */
export function reviewsQuery() {
  return {
    queryKey: queryKeys.reviews,
    queryFn: () => fetchEnvelope<ReviewsView>("/api/reviews"),
    refetchInterval: 15_000,
  };
}

/** The cron registry + last-run health (#483). Cron runs change on the minute at most, so a slow
 * fallback poll is plenty (the SSE `event` frames also nudge the activity feed). */
export function cronQuery() {
  return {
    queryKey: queryKeys.cron,
    queryFn: () => fetchEnvelope<CronView>("/api/cron"),
    refetchInterval: 60_000,
  };
}

/** One cron job's stored findings history (#576) — the durable `{source, run_id, gathered_at,
 * items}` records it appended, newest-first. Feeds the cron detail page's history/timeline. New
 * runs land on the minute at most, so a slow fallback poll is plenty. */
export function findingsQuery(source: string) {
  return {
    queryKey: queryKeys.findings(source),
    queryFn: () =>
      fetchEnvelope<FindingsView>(`/api/findings?source=${encodeURIComponent(source)}`),
    enabled: source.length > 0,
    refetchInterval: 60_000,
  };
}

/** Today's agent's suggested tasks (#577) — the reviewable proposals synthesized from the cron
 * findings, newest-first, optionally narrowed to one status. Feeds the Today page's Suggested
 * tasks section; the manual Run trigger + a slow fallback poll keep it fresh. */
export function suggestionsQuery(status = "") {
  const qs = status ? `?status=${encodeURIComponent(status)}` : "";
  return {
    queryKey: queryKeys.suggestions(status),
    queryFn: () => fetchEnvelope<SuggestionsView>(`/api/suggestions${qs}`),
    refetchInterval: 60_000,
  };
}

/** The triggerable-agent catalog (#557) — every agent/job the automation can fire (crons + native
 * jobs + runnable Markdown agents), with metadata + last-run health, for the Command Center's automation
 * hub. Changes on the minute at most (schedule + last run), so a slow fallback poll is plenty. */
export function catalogQuery() {
  return {
    queryKey: queryKeys.catalog,
    queryFn: () => fetchEnvelope<CatalogView>("/api/agents/catalog"),
    refetchInterval: 60_000,
  };
}

/** Today's scheduled crons (#575) — the plain cron/native jobs whose schedule fires today, each
 * with last-run health + the `job` name the shared `POST /api/cron/run` re-fires. Shares the
 * `CatalogView` shape with the catalog; polls on the minute (schedule + last run only change then). */
export function todayCronsQuery() {
  return {
    queryKey: queryKeys.todayCrons,
    queryFn: () => fetchEnvelope<CatalogView>("/api/today/crons"),
    refetchInterval: 60_000,
  };
}

/** The machine-wide work queue (#queue): pending by kind, nodes, and the last tick's
 * decision. Live via the `queue` SSE frame; the interval is the fallback. */
export function queueQuery() {
  return {
    queryKey: queryKeys.queue,
    queryFn: () => fetchEnvelope<QueueView>("/api/queue"),
    refetchInterval: 30_000,
  };
}

/** The dispatcher's recent history (#queue): every start, stall, reclaim and completion.
 *
 * `queueQuery` answers what is true now; this answers what happened. A queue's failures are mostly
 * things that already happened and left no mark on the current state, which is exactly why the
 * current state cannot show them.
 *
 * Live via the `log` SSE frame, which carries each event as the dispatcher writes it; the interval
 * is the fallback for a dropped stream, not the mechanism. */
export function queueEventsQuery(opts: { since: string; kind: string; repo: string }) {
  const params = new URLSearchParams({ since: opts.since, limit: "2000" });
  if (opts.kind) params.set("kind", opts.kind);
  if (opts.repo) params.set("repo", opts.repo);
  return {
    queryKey: queryKeys.queueEvents(opts.since, opts.kind, opts.repo),
    queryFn: () => fetchEnvelope<QueueEventsView>(`/api/queue/events?${params.toString()}`),
    refetchInterval: 60_000,
  };
}

/** Every backlog entry, in any state (#queue).
 *
 * Separate from `queueQuery`, which carries only the pending set because it rides the SSE frame to
 * every browser on every change. History is asked for far less often than it would be pushed. */
export function queueEntriesQuery(opts: {
  status: string;
  repo: string;
  kind: string;
  class?: string;
}) {
  const params = new URLSearchParams();
  if (opts.status) params.set("status", opts.status);
  if (opts.repo) params.set("repo", opts.repo);
  if (opts.kind) params.set("kind", opts.kind);
  if (opts.class) params.set("class", opts.class);
  return {
    queryKey: queryKeys.queueEntries(opts.status, opts.repo, opts.kind, opts.class ?? ""),
    queryFn: () => fetchEnvelope<QueueEntriesView>(`/api/queue/entries?${params.toString()}`),
    refetchInterval: 60_000,
  };
}

/** The day timeline (#queue) — running / scheduled / projected / done, on one hour axis. The
 * projection replays the dispatcher forward, so it changes as capacity does; a minute is well
 * inside the resolution of a 20-minute mark. */
export function queueTimelineQuery(date?: string, klass?: string) {
  const params = new URLSearchParams();
  if (date) params.set("date", date);
  if (klass) params.set("class", klass);
  const qs = params.toString();
  return {
    queryKey: queryKeys.queueTimeline(date, klass),
    queryFn: () =>
      fetchEnvelope<QueueTimelineView>(qs ? `/api/queue/timeline?${qs}` : "/api/queue/timeline"),
    refetchInterval: 60_000,
  };
}

/** Today's scheduled Markdown agents (#575), each re-triggered through its stable `bn run <name>`
 * identity. */
export function todayAgentsQuery() {
  return {
    queryKey: queryKeys.todayAgents,
    queryFn: () => fetchEnvelope<CatalogView>("/api/today/agents"),
    refetchInterval: 60_000,
  };
}

/** Today's **hosted** agent — the live session in the tmux `planning` session that the Today page
 * streams and steers. Polled briskly (5s): this is the one card whose state changes while you
 * watch it, and a stale `control` channel is the difference between a live Steer composer and a
 * greyed one. */
export function todayAgentQuery() {
  return {
    queryKey: queryKeys.todayAgent,
    queryFn: () => fetchEnvelope<TodayAgentView>("/api/today/agent"),
    refetchInterval: 5_000,
  };
}

/** Every skill across the tiers (#483). Skills change only when edited, so poll slowly. */
export function skillsQuery() {
  return {
    queryKey: queryKeys.skills,
    queryFn: () => fetchEnvelope<SkillsView>("/api/skills"),
    refetchInterval: 60_000,
  };
}

/** One skill's editable source (#483) — the editor loads the raw markdown from here. `enabled`
 * lets the caller defer the fetch until a skill is selected. */
export function skillDetailQuery(name: string | null) {
  return {
    queryKey: queryKeys.skill(name ?? ""),
    queryFn: () => fetchEnvelope<SkillDetail>(`/api/skills/${encodeURIComponent(name ?? "")}`),
    enabled: !!name,
    staleTime: 0,
  };
}

/** Daily + quarter goals (#483). A read; goals move on a human timescale, so poll slowly. */
export function goalsQuery() {
  return {
    queryKey: queryKeys.goals,
    queryFn: () => fetchEnvelope<GoalsView>("/api/goals"),
    refetchInterval: 60_000,
  };
}

/** The local repo inventory (#495): local repos plus their per-repo rollup and session list. A fold
 * over local clone state / Notes / Fleet / Orchestrate / Cost, so a slow fallback poll is plenty
 * (the SSE `fleet`/`orchestrate` frames keep the underlying live data fresh on the Fleet surface). */
export function reposQuery() {
  return {
    queryKey: queryKeys.repos,
    queryFn: () => fetchEnvelope<ReposView>("/api/repos"),
    refetchInterval: 30_000,
  };
}

export function repoScriptQuery(repo: string, name: string) {
  return {
    queryKey: queryKeys.repoScript(repo, name),
    queryFn: () =>
      fetchEnvelope<RepoScriptView>(
        `/api/repos/${encodeURIComponent(repo)}/scripts/${encodeURIComponent(name)}`,
      ),
    enabled: repo.length > 0 && name.length > 0,
  };
}

/** One repo's open GitHub issues (#556) — best-effort via `gh` server-side, so it can be a touch
 * slow and empty when offline/unauthed. A slow poll keeps the pick-list fresh without hammering gh. */
export function repoIssuesQuery(repo: string) {
  return {
    queryKey: queryKeys.repoIssues(repo),
    queryFn: () => fetchEnvelope<RepoIssuesView>(`/api/repos/${encodeURIComponent(repo)}/issues`),
    enabled: repo.length > 0,
    refetchInterval: 120_000,
  };
}

/** The ⌘K command-palette search (#565). On-demand and keyed by the trimmed query: the palette
 * debounces upstream and only enables the query for a non-empty box. `gh` issue enrichment can make
 * a response a touch slow, so the last results stay visible while the next query loads
 * (`keepPreviousData`) and there's no background refetch — a palette is a point-in-time lookup. */
export function searchQuery(q: string) {
  const query = q.trim();
  return {
    queryKey: queryKeys.search(query),
    queryFn: () => fetchEnvelope<SearchView>(`/api/search?q=${encodeURIComponent(query)}`),
    enabled: query.length > 0,
    staleTime: 10_000,
    placeholderData: keepPreviousData,
  };
}

/** The learnings feed filter — mirrors `GET /api/learnings?since=&all=&repo=&skill=&agent=`. */
export interface LearningsFilter {
  since?: number | null;
  all?: boolean;
  repo?: string;
  skill?: string;
  /** `agent` | `mine`; empty = any author. */
  agent?: string;
}

/** The learnings review feed for a filter (the Notes page's Learnings tab). */
export function learningsQuery(filter: LearningsFilter) {
  const p = new URLSearchParams();
  if (filter.since != null) p.set("since", String(filter.since));
  if (filter.all) p.set("all", "true");
  if (filter.repo) p.set("repo", filter.repo);
  if (filter.skill) p.set("skill", filter.skill);
  if (filter.agent) p.set("agent", filter.agent);
  const qs = p.toString();
  return {
    queryKey: queryKeys.learnings(filter),
    queryFn: () => fetchEnvelope<LearningsView>(`/api/learnings${qs ? `?${qs}` : ""}`),
    refetchInterval: 30_000,
  };
}

/** The Today view's "achieved": today's end-of-day digest — branches touched, un-noted commits,
 * PRs merged today, still-open todos, carry-forward (`GET /api/wrap`, mirroring `bn wrap`). It
 * shells git/gh server-side, so it polls on a slow fallback cadence. */
export function wrapQuery() {
  return {
    queryKey: queryKeys.wrap,
    queryFn: () => fetchEnvelope<WrapView>("/api/wrap"),
    refetchInterval: 60_000,
  };
}

/** The `GET /api/metrics` velocity + throughput scorecard (#507) for a trailing `windowDays`
 * window. It reads the whole event log + shells git server-side, so it polls on a slow fallback. */
export function metricsQuery(windowDays: number) {
  return {
    queryKey: queryKeys.metrics(windowDays),
    queryFn: () => fetchEnvelope<MetricsView>(`/api/metrics?window=${windowDays}`),
    refetchInterval: 60_000,
  };
}

/** The `GET /api/metrics/trend` snapshot history behind the trend chart (#507). */
export function metricsTrendQuery() {
  return {
    queryKey: queryKeys.metricsTrend,
    queryFn: () => fetchEnvelope<MetricsTrendView>("/api/metrics/trend"),
    refetchInterval: 60_000,
  };
}

/** The `GET /api/insights` weekly auto-retro digest (#507). */
export function insightsQuery() {
  return {
    queryKey: queryKeys.insights,
    queryFn: () => fetchEnvelope<InsightsView>("/api/insights"),
    refetchInterval: 60_000,
  };
}

/** The `GET /api/weekly` 1-1 review digest (#507). */
export function weeklyQuery() {
  return {
    queryKey: queryKeys.weekly,
    queryFn: () => fetchEnvelope<WeeklyView>("/api/weekly"),
    refetchInterval: 60_000,
  };
}

/** The `GET /api/brag` accomplishments / impact packet (#507). Shells git/gh server-side, so it
 * polls on a slow fallback cadence. */
export function bragQuery() {
  return {
    queryKey: queryKeys.brag,
    queryFn: () => fetchEnvelope<BragView>("/api/brag"),
    refetchInterval: 60_000,
  };
}

/** How long Today's reads stay fresh.
 *
 * The global default is `staleTime: 5_000`, which is right for a screen you sit on and wrong for
 * one you navigate back to: every return to `/` refired six queries and repainted the lists from
 * their loading states. These windows are set from how fast each source *actually* changes, so a
 * remount inside the window is served from cache with no request at all.
 *
 * The SSE stream only does a blanket invalidate when it reconnects after a drop (`useBnStream`),
 * so `refetchInterval` still carries liveness — this caches *between* those polls, it does not
 * replace them. `gcTime` is deliberately much longer than any window: keeping a few KB of JSON so
 * a back-navigation paints instantly is a trade worth making every time.
 */
const TODAY_CACHE = {
  /** The task store and the pipeline change only when you or an agent act. */
  work: { staleTime: 15_000, gcTime: 10 * 60_000 },
  /** A day's finished record barely moves, and past days never do. */
  done: { staleTime: 60_000, gcTime: 30 * 60_000 },
  /** The server already holds a TTL cache for issues (#999) — polling faster just re-serves it. */
  slow: { staleTime: 120_000, gcTime: 30 * 60_000 },
} as const;

/** Keep the previous page of data on screen while a refetch is in flight.
 *
 * Without this, every background refetch drops the lists back to `undefined` for a frame and the
 * screen flashes its empty states — "Nothing is ready", "Nothing is in flight" — which reads as
 * *the work disappeared*, not as *we are reloading*. */
const keepPrevious = <T>(prev: T | undefined) => prev;

/** Today's tasks — `day:` today-or-earlier plus unscheduled, mirroring the CLI's bare view so the
 * dashboard and `bn task` never disagree about what "today" means. */
/** One task plus everything already joined to it: the live agent on its worktree, its branch note,
 * and its context doc. Polls at the read cadence — a task page is a place you sit and watch. */
export function taskQuery(id: string) {
  return {
    queryKey: queryKeys.task(id),
    queryFn: () => fetchEnvelope<TaskDetailView>(`/api/tasks/${encodeURIComponent(id)}`),
    refetchInterval: 15_000,
    ...TODAY_CACHE.work,
  };
}

export function tasksQuery(scope: "today" | "all" = "today") {
  return {
    queryKey: queryKeys.tasks(scope),
    queryFn: () => fetchEnvelope<TasksView>(`/api/tasks${scope === "all" ? "?all=1" : ""}`),
    refetchInterval: 30_000,
    placeholderData: keepPrevious<TasksView>,
    ...TODAY_CACHE.work,
  };
}

export type TaskLedgerState = "all" | "open" | "wip" | "snoozed" | "done" | "cancelled";

/** The task store's complete history, bounded and searched on the server. */
export function taskLedgerQuery(opts: {
  state?: TaskLedgerState;
  query?: string;
  repo?: string;
  offset?: number;
  limit?: number;
}) {
  const params = new URLSearchParams({
    status: "all",
    limit: String(opts.limit ?? 50),
    offset: String(opts.offset ?? 0),
  });
  if (opts.state && opts.state !== "all") params.set("state", opts.state);
  if (opts.query) params.set("q", opts.query);
  if (opts.repo) params.set("repo", opts.repo);
  const scope = `ledger:${opts.state ?? "all"}:${opts.query ?? ""}:${opts.repo ?? ""}:${opts.offset ?? 0}`;
  return {
    queryKey: queryKeys.tasks(scope),
    queryFn: () => fetchEnvelope<TaskLedgerView>(`/api/tasks?${params}`),
    refetchInterval: 60_000,
    placeholderData: keepPrevious<TaskLedgerView>,
    ...TODAY_CACHE.work,
  };
}

/** The queue's reservoir list — open tasks, narrowed by SMART verdict and repo.
 *
 * Separate from [`tasksQuery`] because the queue asks a different question than Today does: not
 * "what am I doing today" but "what is waiting, and how far is it from being runnable". `smart`
 * maps onto the gate's verdicts (`""` means every open task); the server filters, so a store with
 * thousands of unjudged tasks does not cross the wire to answer "how many are ready". */
export function reservoirQuery(opts: { smart?: string; repo?: string } = {}) {
  const params = new URLSearchParams({ all: "1" });
  if (opts.smart) params.set("smart", opts.smart);
  if (opts.repo) params.set("repo", opts.repo);
  const qs = params.toString();
  return {
    queryKey: ["queue", "reservoir", opts.smart ?? "", opts.repo ?? ""] as const,
    queryFn: () => fetchEnvelope<TasksView>(`/api/tasks?${qs}`),
    refetchInterval: 30_000,
    placeholderData: keepPrevious<TasksView>,
    ...TODAY_CACHE.work,
  };
}

/** Every repo's open issues — the reservoir tasks are pulled from. Polls slowly: the server holds
 * a TTL cache (#999), so a tight poll would only re-serve the same bytes. */
export function issuesQuery() {
  return {
    queryKey: queryKeys.issues,
    queryFn: () => fetchEnvelope<IssuesView>("/api/issues"),
    refetchInterval: 120_000,
    placeholderData: keepPrevious<IssuesView>,
    ...TODAY_CACHE.slow,
  };
}

/** The nine-stage pipeline board with per-stage counts. */
export function pipelineQuery() {
  return {
    queryKey: queryKeys.pipeline,
    queryFn: () => fetchEnvelope<PipelineBoardView>("/api/pipeline/board"),
    refetchInterval: 30_000,
  };
}

/** The pipeline as *work*: each stage with the tasks actually sitting in it, their live agents,
 * and any orphan worktrees.
 *
 * This replaces drawing `/api/pipeline/board`'s counts next to a separately-queried task list.
 * Those were two different populations joined by a filter on `task.stage`, a field nothing ever
 * writes — so a node could read `3` above the words "Nothing is in execute". Here the count and
 * the rows are one value, which is why the component can no longer render them out of step. */
export function pipelineStagesQuery() {
  return {
    queryKey: queryKeys.pipelineStages,
    queryFn: () => fetchEnvelope<PipelineStagesView>("/api/pipeline/stages"),
    refetchInterval: 30_000,
    // The most expensive read on the page — it folds the event log, the task store *and* a fleet
    // supervise pass — and it is the one the stage drill-down reads from. Caching it means
    // clicking through stages costs nothing: the filtering is already client-side over this one
    // payload, so no stage selection ever issues a request.
    placeholderData: keepPrevious<PipelineStagesView>,
    ...TODAY_CACHE.work,
  };
}

/** What was finished on a day — completions *and* cancellations, kept apart. Defaults to today. */
export function doneTasksQuery(day?: string) {
  return {
    queryKey: queryKeys.doneTasks(day ?? "today"),
    queryFn: () =>
      fetchEnvelope<DoneTasksView>(
        `/api/tasks?status=done${day ? `&day=${encodeURIComponent(day)}` : ""}`,
      ),
    // A past day is immutable: nothing can be completed on a day that has already ended, so it is
    // fetched once per session and never again. Only today's record keeps polling.
    ...(day
      ? { staleTime: Infinity, gcTime: TODAY_CACHE.done.gcTime }
      : { refetchInterval: 60_000, ...TODAY_CACHE.done }),
    placeholderData: keepPrevious<DoneTasksView>,
  };
}
