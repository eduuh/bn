// Typed viewmodels for the bn-web read API. Every shape here mirrors a `bn-core` payload builder
// exactly (design §3), so SSE frames and REST envelopes share one contract.

/** The standard bn JSON envelope wrapping every REST response (`bn_core::render::Envelope`). */
export interface Envelope<T> {
  ok: boolean;
  command: string;
  data: T;
  error: { code: string; message: string } | null;
}

// --- Fleet (GET /api/fleet .data; SSE `fleet` frame is this shape raw) ---

export type AgentHealth = "starting" | "active" | "idle" | "stuck";

/** An agent's fleet role — `bn_core::agent::AgentRole` (#500). Orchestrators run the wave and pin
 * to their own top tier; workers ship PRs and render normally. Absent on pre-#500 payloads. */
export type AgentRole = "worker" | "orchestrator";

/** One live agent — `bn_core::agent::LiveAgent`, serialized verbatim. */
export interface LiveAgent {
  session_id: string;
  harness: string;
  repo: string;
  branch: string;
  /** `repo:branch` — the identity every bn command addresses this agent by. */
  target: string;
  /**
   * The exact `session:window` hosting this agent, when the supervisor's join found one. A window
   * is often named after a pooled slot or a reused directory rather than the branch, so this is not
   * derivable from `target`. Absent on older servers.
   */
  tmux_target?: string;
  /** The agent's real checkout, from the transcript cwd refined by the window/lease join. */
  worktree?: string;
  last_activity_ts: number;
  idle_secs: number;
  health: AgentHealth;
  /** Worker vs orchestrator (#500); defaults to `worker` when absent. */
  role?: AgentRole;
  /** Launch-path work class, when bn launched or adopted this session. */
  work_class?: WorkClass | null;
  /** Readable task/workflow name recorded at launch (`bn dispatch --name`, controller name, etc.). */
  task_name?: string | null;
  turns: number;
  tool_calls: number;
  input_tokens: number;
  output_tokens: number;
  cache_read_tokens: number;
  /**
   * How bn can write to this agent — `resume` (a recorded session), `tmux` (a live window), or
   * `none`. Supervision observes every running session on the machine, including ones bn never
   * launched, so appearing in the fleet does not make an agent steerable. Absent on older servers,
   * which is treated as unknown-but-allowed so the panel doesn't lock out against a stale binary.
   */
  control?: AgentControl;
  /**
   * The task this agent is working on — `bn_core::agent::AgentTask`. Present when a task is bound
   * to the agent's `(repo, branch)` (`bn dispatch --task`, `bn task claim`); absent otherwise, and
   * absent on older servers. An agent with no task is a normal state, not an error.
   */
  task?: AgentTask;
  plan?: import("./plan-types").AgentPlanSummary;
}

/** `bn_core::agent::AgentTask` — enough of a task to recognize it and link to it. */
export interface AgentTask {
  /** The positional handle (`t42`) — what `/tasks/:id` is addressed by. */
  id: string;
  summary: string;
  stage?: string;
  /** The SMART gate's verdict, when judged. */
  smart?: string;
}

/** `bn_core::agent::ControlChannel`. */
export type AgentControl = "resume" | "tmux" | "ambiguous" | "none";

/** `bn_core::agent::Fleet` — agents plus health counts, plus #501 harness availability. */
export interface FleetView {
  agents: LiveAgent[];
  /** Just-dispatched agents with no transcript yet (#545) — the pending "starting…" tier. */
  starting?: number;
  active: number;
  idle: number;
  stuck: number;
  total_tokens: number;
  /** Per-machine harness availability for the switcher (copilot always; claude may be disabled). */
  harnesses?: HarnessOption[];
}

// --- Machine health (GET /api/machine .data) — #525 ---

/** Coarse traffic-light for the machine-health widget (`bn_core::machine::Level`). */
export type MachineLevel = "ok" | "warn" | "critical";

/** CPU load snapshot (`bn_core::machine::Cpu`). Load figures are best-effort — `null` when
 * unreadable (never a false alarm). `load_per_core` is `load1 / cores`, the normalized gauge. */
export interface MachineCpu {
  load1: number | null;
  load5: number | null;
  load15: number | null;
  cores: number;
  load_per_core: number | null;
}

/** RAM snapshot in MB (`bn_core::machine::Ram`). `swap_used_mb` is the real OOM signal. Fractions
 * are `null` when the denominator is unknown (no swap configured, or total unreadable). */
export interface MachineRam {
  total_mb: number;
  free_mb: number;
  used_mb: number;
  swap_total_mb: number;
  swap_used_mb: number;
  used_frac: number | null;
  swap_used_frac: number | null;
}

/** The derived near-saturation state (`bn_core::machine::Saturation`). */
export interface MachineSaturation {
  level: MachineLevel;
  reasons: string[];
}

/** `GET /api/machine` .data (#525) — the host the fleet runs on + how many agents load it. */
export interface MachineView {
  cpu: MachineCpu;
  ram: MachineRam;
  saturation: MachineSaturation;
  agents: { active: number };
}

// --- Nodes (GET /api/nodes .data) ---

/** Reachability of one fleet machine. The local node is always `online`; remote nodes go
 * `unreachable` when their heartbeat lapses. */
export type NodeStatus = "online" | "unreachable";

/** One fleet machine: the `/api/machine` snapshot plus identity and reachability. */
export interface NodeView extends MachineView {
  host: string;
  /** `std::env::consts::OS` on the node — "macos", "linux", "windows", … */
  platform: string;
  /** `std::env::consts::ARCH` on the node — "aarch64", "x86_64", … */
  arch: string;
  status: NodeStatus;
  /** True for the node running this server. */
  local: boolean;
}

/** `GET /api/nodes` .data — a list from day one, so remote nodes join without a contract change. */
export interface NodesView {
  nodes: NodeView[];
}

/** One harness option for the per-agent switcher (#501) — `bn_core::harness::HarnessOption`.
 * `available:false` renders it disabled with `reason` as a tooltip (never a silent drop). */
export interface HarnessOption {
  name: string;
  available: boolean;
  reason?: string;
}

// --- Orchestrate (GET /api/orchestrate .data; SSE `orchestrate` frame is this raw) ---

/** The #340 classified pane state, kebab-cased (`bn_core::orchestrate::PaneState`). */
export type PaneState =
  | "working"
  | "idle-clean"
  | "idle-with-wip"
  | "trust-menu"
  | "command-approval"
  | "question-menu"
  | "api-error"
  | "dns-outage"
  | "limited"
  | "done"
  | "dead";

/** Queue-item lifecycle status, kebab-cased (`bn_core::orchestrate::ItemStatus`). */
export type ItemStatus = "queued" | "dispatched" | "merged" | "failed";

/** The recovery action taken, kebab-cased (`bn_core::orchestrate::SupervisorAction`). */
export type SupervisorAction =
  | "leave"
  | "land"
  | "answer-trust-menu"
  | "answer-command-approval"
  | "answer-question"
  | "relaunch-to-resume"
  | "switch-harness";

/** One queued work item — `bn_core::orchestrate::QueueItem`. */
export interface QueueItem {
  id: string;
  item: string;
  status: ItemStatus;
  dispatch_phase?: "pending" | "claimed" | "dispatched";
  claimed_remote?: boolean;
  branch?: string | null;
  pr?: string | null;
  attempts: number;
  dispatched_at: number;
  last_relaunch_at: number;
  updated_at: number;
  last_state?: PaneState | null;
  // #372: navigation coordinates from `payload::orchestrate_status_data`. `repo` is always set;
  // `tmux_session`/`tmux_window` (session = repo, window = sanitized branch) and `harness` land
  // once the item is dispatched. The board still joins the fleet for live tokens.
  repo?: string;
  tmux_session?: string;
  tmux_window?: string;
  harness?: string;
  // #349: set on a dispatched item when a plan-review for its repo+branch is still `pending` —
  // the agent is paused waiting-for-review. `review_id` links to that review.
  waiting_for_review?: boolean;
  review_id?: string;
  // #376: while queued, the id of the in-flight item whose touch-area this one overlaps (why it's
  // held rather than dispatched). #377: where the item runs — "auto" | "local" | "remote".
  held_by?: string | null;
  substrate?: "auto" | "local" | "remote";
}

export interface Counts {
  queued: number;
  in_flight: number;
  merged: number;
  failed: number;
  total: number;
  /** #377: of `in_flight`, how many run on a remote runner (occupying no local slot). */
  remote?: number;
}

/** One recovery / lifecycle event (`bn_core::orchestrate::RecoveryEvent`); `repo` is added by the
 * `/api/orchestrate/recovery` endpoint's cross-run fold. */
export interface RecoveryEvent {
  ts: number;
  id: string;
  state: PaneState;
  action: SupervisorAction;
  note: string;
  repo?: string;
}

/** One orchestration run — `bn_core::payload::orchestrate_status_data`. */
export interface RunView {
  repo: string;
  concurrency: number;
  solo: boolean;
  stopped: boolean;
  drained: boolean;
  started_at: number;
  updated_at: number;
  counts: Counts;
  /** #377: the binding cap label ("ram"|"load"|"tokens"|"cost"|"local-cap") when local dispatch
   * was capped/paused on the last pass, else absent. Why pending items aren't dispatching. */
  dispatch_bind?: string | null;
  queue: QueueItem[];
  /** Newest-first, capped at 20 by the payload. */
  recovery: RecoveryEvent[];
}

/** GET /api/orchestrate .data — every run on the machine (or one when `?repo=`). */
export interface OrchestrateView {
  runs: RunView[];
}

/** GET /api/orchestrate/recovery .data — cross-run recovery events, newest-first. */
export interface RecoveryView {
  recovery: RecoveryEvent[];
}

// --- Agent detail (GET /api/agents/:session_id .data) ---

/** One recorded agent session — `bn_core::agent::AgentSession`, serialized verbatim. The detail
 * header's post-mortem stats (turns, tokens, models, outcome) when a live session isn't running. */
export interface AgentSession {
  session_id: string;
  harness: string;
  repo: string;
  branch: string;
  started_ts: number;
  ended_ts: number;
  turns: number;
  tool_calls: number;
  permission_prompts: number;
  models: string[];
  input_tokens: number;
  output_tokens: number;
  cache_read_tokens: number;
  cost_nanoaiu: number | null;
  lines_added: number;
  lines_removed: number;
  files_modified: number;
  outcome: string;
  work_class: WorkClass;
}

/** GET /api/agents/:session_id .data — the live fleet entry, its most-recent recorded session, the
 * text of its last request, and the rendered branch note (all may be absent for a session
 * mid-flight or a branch with no note). */
export interface AgentDetailView {
  agent: LiveAgent;
  latest_session: AgentSession | null;
  last_request: string | null;
  /** The session's branch note (typed frontmatter + prose body + goal), or `null` if none. */
  note: NoteDetail | null;
  /** Per-machine harness availability for the detail switcher (#501). */
  harnesses?: HarnessOption[];
}

/** What one activity row represents — `bn_core::agent::EntryKind`. */
export type EntryKind = "user" | "agent" | "tools";

/** One tool invocation inside a collapsed run. */
export interface ToolCall {
  /** Display-cased tool name ("Read", "Bash", "Edit"). */
  name: string;
  /** The identifying argument — a path, command, or pattern. May be empty. */
  target: string;
}

/** One row of the session timeline — `bn_core::agent::ActivityEntry`. */
export interface ActivityEntry {
  kind: EntryKind;
  /** Epoch seconds of the underlying transcript event (0 when it carried no timestamp). */
  ts: number;
  /** Markdown prose for `user` / `agent` rows; empty for a tool run. */
  text: string;
  /** The calls behind a `tools` row; empty otherwise. */
  tools: ToolCall[];
  /** One-line description of a tool run ("14 tool calls · Read ×6"); empty otherwise. */
  summary: string;
}

/** GET /api/agents/:session_id/activity .data — the session transcript as a readable timeline.
 * Replaces the pane mirror as the agent-detail supervise surface: it reads the harness transcript
 * off disk, so it still answers once the agent's tmux window has closed. */
export interface ActivityView {
  session_id: string;
  /** "claude" | "copilot". */
  harness: string;
  /** The trailing entries, oldest first. */
  entries: ActivityEntry[];
  /** Total entries in the session, so the UI can say "showing the last 40 of 312". */
  total: number;
  count: number;
}

// --- Notes (GET /api/notes .data; GET /api/notes/:repo/:branch .data) ---

/** A todo / blocker reference within a note — `bn_core::rollups::ItemRef`. */
export interface ItemRef {
  id: string;
  text: string;
  line: number;
  /** Creation date (`YYYY-MM-DD`) — the recency signal the action center ranks todos by. */
  created: string;
}

/** One branch-note rollup row — `bn_core::rollups::SummaryEntry`. */
export interface SummaryEntry {
  repo: string;
  branch: string;
  path: string;
  status: string;
  todos: ItemRef[];
  blockers: ItemRef[];
}

/** GET /api/notes .data — every branch note's rollup (`payload::summary_data`). */
export interface NotesView {
  branches: SummaryEntry[];
}

/** One structured note item — `bn_core::model::Item`. Todos/done-when carry `done`; any item may
 * carry a `link` (an ADO work item, a doc). */
export interface NoteItem {
  id: string;
  text: string;
  created: string;
  done?: boolean | null;
  done_at?: string | null;
  link?: string | null;
  pct?: number | null;
}

/** The typed note frontmatter — `bn_core::model::Frontmatter`, serialized verbatim. `pr`/`work_item`
 * are YAML scalars (a bare number round-trips as a JSON number). The six structured sections plus
 * `done_when` live here in the single-file layout; the prose (Goal/Progress/…) is in `body`. */
export interface NoteFront {
  repo: string;
  branch: string;
  created: string;
  status: string;
  machine?: string;
  type?: string | null;
  work_item?: string | number | null;
  pr?: string | number | null;
  tags: string[];
  collaborators: string[];
  harnesses?: string[];
  models?: string[];
  todos: NoteItem[];
  blockers: NoteItem[];
  decisions: NoteItem[];
  research: NoteItem[];
  collab: NoteItem[];
  ask: NoteItem[];
  done_when: NoteItem[];
}

/** GET /api/notes/:repo/:branch .data — one full note: typed frontmatter, prose body, goal line. */
export interface NoteDetail {
  repo: string;
  branch: string;
  front: NoteFront;
  body: string;
  goal: string;
}

// --- Handoff (GET/POST /api/handoff) — the per-branch resume packet (#509) ---

/** Layer 1 — the state snapshot lifted from the note (`bn_core::handoff::StateSnapshot`). */
export interface HandoffState {
  goal?: string | null;
  progress: string[];
  open_todos: string[];
  blockers: string[];
}

/** Layer 3 — decisions made (from the note) + deferred (author-supplied). */
export interface HandoffDecisions {
  made: string[];
  deferred: string[];
}

/** The structured resume packet — `bn_core::handoff::Handoff`, serialized verbatim. */
export interface Handoff {
  repo: string;
  branch: string;
  written: string;
  state: HandoffState;
  narrative: string;
  decisions: HandoffDecisions;
  open_questions: string[];
  priority_queue: string[];
}

/** GET/POST /api/handoff .data — the packet plus its rendered markdown mirror + orientation banner. */
export interface HandoffView {
  handoff: Handoff;
  markdown: string;
  rendered: string;
}

// --- Repos (GET /api/repos .data) — multi-repo navigation (#495) ---

export {
  MAX_POOL_SIZE,
  MIN_POOL_SIZE,
  type Forge,
  type ForgeLinks,
  type RepoPool,
  type RepoPoolSlot,
  type RepoRollup,
  type RepoScriptView,
  type RepoSession,
  type RepoSetup,
  type RepoSetupScript,
  type RepoSetupStatus,
  type RepoSetupVerify,
  type RepoVerifyMode,
  type ReposView,
} from "./repo-types";

/** One open GitHub issue for a repo — `bn_core::palette::IssueHit` (#556). */
export interface RepoIssue {
  number: number;
  title: string;
  url: string;
  state: string;
}

/** GET /api/repos/:repo/issues .data — a repo's open GitHub issues (#556). Best-effort: `slug` is
 * the resolved `owner/repo` (or `null` when the repo's remote couldn't be resolved), and `issues`
 * is empty on any gh/git failure. */
export interface RepoIssuesView {
  repo: string;
  slug: string | null;
  issues: RepoIssue[];
}

// --- Attention (GET /api/attn .data) ---

/** One open attention item — `bn_core::attention::AttnItem`. A `waiting-for-review` plan request
 * (source `agent`, a `detail`/`prompt` plan body) drives the agent-detail plan-review panel. */
export interface AttnItem {
  id: string;
  source: string;
  title: string;
  severity: string;
  repo?: string | null;
  branch?: string | null;
  link?: string | null;
  key?: string | null;
  detail?: string | null;
  mode?: string | null;
  prompt?: string | null;
  created: number;
}

/** GET /api/attn .data — open attention items for today (`payload::attn_list_data`). */
export interface AttnView {
  items: AttnItem[];
}

// --- Action center (GET /api/actions .data; #522) ---

/** The kind of an action-center item — `bn_core::action_center::ActionKind`, kebab-cased. Drives
 * the row icon, the prompt template, and the default urgency. */
export type ActionKind =
  | "attention"
  | "plan-review"
  | "todo"
  | "blocker"
  | "failing-cron"
  | "stale-pr"
  | "goal"
  | "issue"
  | "suggestion";

/** Whether the Start-agent composer opens on dispatch-now or queue — `action_center::Urgency`. */
export type Urgency = "urgent" | "queued";

/** One unified actionable item — `bn_core::action_center::ActionItem`. Each carries a per-type
 * editable `suggested_prompt` and a `default_urgency`, so any row can one-click Start an agent with
 * a real starting-point instruction pre-filled. `payload` carries the dispatch coordinates
 * (`repo`/`branch`/`target` + a type-specific id). */
export interface ActionItem {
  id: string;
  /** The item's origin kind (serialized as `type`). */
  type: ActionKind;
  title: string;
  source: string;
  /** `urgent` | `warn` | `info` — tone + sort. */
  severity: string;
  repo?: string | null;
  branch?: string | null;
  link?: string | null;
  detail?: string | null;
  /** Dispatch hints: `{ repo?, branch?, target?, review_id?, cron?, pr?, attn_id?, goal_id? }`. */
  payload: Record<string, unknown>;
  /** The pre-filled, editable agent prompt (a per-type template). */
  suggested_prompt: string;
  default_urgency: Urgency;
  created: number;
  /** True for a stale, low-signal backlog todo (created past the staleness threshold, or undated).
   * Always `false` for non-todo kinds. The UI de-emphasizes these and folds them under the todo
   * section's "show all" expander so urgent work stays visible. */
  stale: boolean;
}

/** GET /api/actions .data — the ranked unified action list (`payload::actions_data`). `cron_activity`
 * (#557) is the observational feed of recent cron runs (ok + failed), its own Command Center section
 * — distinct from the actionable `failing-cron` items. */
export interface ActionsView {
  count: number;
  actions: ActionItem[];
  cron_activity: CronActivity[];
}

/** One row in the cron-activity feed — `bn_core::action_center::CronActivity` (#557). A job the
 * automation has actually *run*, with its most-recent outcome; ok + failed alike, most-recent-first.
 * `severity` is `urgent` when the last run failed, else `info`. */
export interface CronActivity {
  job: string;
  scope: string;
  ok: boolean;
  last_exit?: number | null;
  last_run: number;
  runs: number;
  failures: number;
  declared: boolean;
  schedule?: string | null;
  description?: string | null;
  /** The last run's captured output tail — present only when that run failed. */
  last_output?: string | null;
  severity: string;
}

// --- Completed tab (GET /api/completed .data; #561) ---

/** The kind of a completed item — `bn_core::completed::CompletedKind`, kebab-cased. Drives the
 * row's icon + group label. */
export type CompletedKind = "todo" | "goal" | "attention" | "merged-pr";

/** A branch note's two jump locations — `bn_core::completed::NoteLink`. Both are best-effort: a
 * note absent locally, or a notes repo with no git remote, leaves the corresponding field unset. */
export interface NoteLink {
  /** Absolute path of the branch note on this machine (the "open locally" jump). */
  local_path?: string | null;
  /** Web URL to the branch note in the synced remote (the "open remotely" jump). */
  remote_url?: string | null;
}

/** One finished task — `bn_core::completed::CompletedItem`. `completed` is epoch-seconds of
 * completion (the list is most-recent-first); `note` carries the local + remote jumps back to the
 * branch note that holds its full context. */
export interface CompletedItem {
  id: string;
  /** The item's origin kind (serialized as `type`). */
  type: CompletedKind;
  title: string;
  source: string;
  repo?: string | null;
  branch?: string | null;
  /** A deep link when the item has one (a merged PR url, a goal's attached link). */
  link?: string | null;
  detail?: string | null;
  /** Epoch-seconds the item completed — the recency sort key. */
  completed: number;
  note: NoteLink;
}

/** GET /api/completed .data — the most-recent-first completed history (`payload::completed_data`). */
export interface CompletedView {
  count: number;
  completed: CompletedItem[];
}

// --- Global search / command palette (GET /api/search .data; #565) ---

/** The presentation kind of a palette result — `bn_core::palette::ResultKind`, kebab-cased. Drives
 * the row icon + group heading. The first nine mirror `ActionKind` (they carry a "work on"
 * `action`); the rest are navigational jump targets. */
export type SearchResultKind =
  | "todo"
  | "blocker"
  | "attention"
  | "plan-review"
  | "failing-cron"
  | "stale-pr"
  | "goal"
  | "issue"
  | "suggestion"
  | "note"
  | "learning"
  | "agent"
  | "repo"
  | "cron"
  | "skill";

/** One typed, actionable palette result — `bn_core::palette::SearchResult`. When `action` is
 * present the result can "work on this" (it feeds the reused `StartAgentComposer` verbatim);
 * `route` is an in-app SPA path to jump to; `link` an external URL to open in a new tab. */
export interface SearchResult {
  id: string;
  type: SearchResultKind;
  title: string;
  subtitle: string;
  repo?: string | null;
  branch?: string | null;
  link?: string | null;
  route?: string | null;
  action?: ActionItem | null;
  score: number;
}

/** GET /api/search .data — the ranked palette result set (`payload::palette_data`). `query` echoes
 * the request so a slow response can be dropped if the box has moved on. */
export interface SearchView {
  query: string;
  count: number;
  results: SearchResult[];
}

// --- Plan-reviews (GET /api/reviews .data; #349) ---

/** A plan-review's lifecycle status — `bn_core::plan_review::ReviewStatus`. */
export type ReviewStatus = "pending" | "approved" | "changes-requested" | "rejected";

/** One persisted plan-review record — `bn_core::plan_review::Review`. The dashboard's plan-review
 * panel renders `plan` and drives approve / request-changes / reject on `id`. */
export interface Review {
  id: string;
  repo: string;
  branch: string;
  session?: string | null;
  plan: string;
  status: ReviewStatus;
  notes?: string | null;
  attn_id?: string | null;
  created: number;
  updated: number;
}

/** GET /api/reviews .data — every plan-review, newest first (`payload::plan_reviews_data`). */
export interface ReviewsView {
  reviews: Review[];
}

// --- Cron registry (GET /api/cron .data; #483) ---

export type { AgentSchedule, CronJob, CronView } from "./cron-types";

// --- Cron findings (GET /api/findings .data; #576) ---

/** One gathered item — `bn_core::feed::FeedItem`, the `{key,text,url}` shape shared by the feed and
 * the findings store. */
export interface FeedItem {
  key: string;
  text: string;
  url: string;
}

/** One run's findings — `bn_core::findings::FindingsRun` (#576): what a cron gathered, when, from
 * where. `items` reuses the feed `{key,text,url}` shape; `raw` optionally carries the untransformed
 * payload for later re-parsing. */
export interface FindingsRun {
  source: string;
  run_id: string;
  /** Epoch-seconds the run gathered these findings. */
  gathered_at: number;
  items: FeedItem[];
  raw?: string | null;
}

/** GET /api/findings .data — recorded cron findings, newest-first (`api::findings`). */
export interface FindingsView {
  count: number;
  runs: FindingsRun[];
}

// --- Today's agent suggestions (Today's agent, #577) ---

/** Where a suggested task sits in its review lifecycle — `bn_core::suggestions::SuggestionStatus`. */
export type SuggestionStatus = "suggested" | "accepted" | "dismissed";

/** One proposed task synthesized from the cron findings — `bn_core::suggestions::Suggestion`
 * (#577): what to do (`title`), why (`rationale`), and where it came from (`source` + `source_key`,
 * plus an optional `url` to the finding). `id` is deterministic on `source`+`source_key`. */
export interface Suggestion {
  id: string;
  title: string;
  rationale: string;
  source: string;
  source_key: string;
  url: string;
  /** Epoch-seconds this suggestion was first created. */
  created_at: number;
  status: SuggestionStatus;
}

/** GET /api/suggestions .data — Today's agent's suggested tasks, newest-first (`api::suggestions`). */
export interface SuggestionsView {
  count: number;
  suggestions: Suggestion[];
}

/** What one Today's-agent run did — `bn_core::today_agent::RunReport` (#577). */
export interface TodayRunReport {
  /** Distinct findings sources the packet drew from. */
  sources: number;
  /** Finding items considered across those sources. */
  items: number;
  /** Newly written suggestions (existing/dismissed ones are skipped). */
  added: number;
}

/** POST /api/today/run .data — the run report plus the refreshed open suggestions (`actions::today_run`). */
export interface TodayRunResult {
  report: TodayRunReport;
  suggestions: Suggestion[];
}

/** What bn recorded when it launched Today's hosted agent — `bn_core::today_agent::AgentRecord`.
 * `target` is the `repo:branch` every steering write is keyed by (`today:<date>`); `tmux_target`
 * is the `session:window` you can drop into by hand. */
export interface TodayAgentRecord {
  date: string;
  session_id: string;
  harness: string;
  repo: string;
  branch: string;
  target: string;
  tmux_target: string;
  cwd: string;
  status: "starting" | "running" | "stopped";
  started_at: number;
  stopped_at?: number;
}

/** GET /api/today/agent .data — Today's hosted agent (`bn_core::today_agent::status_view`).
 *
 * `running` comes from the **live fleet**, not from `agent.status`: the record says what bn
 * launched, the supervisor says what is alive. A recorded agent whose session has died reads as
 * `running: false` with the record still present, which is what lets the card say "it's gone"
 * rather than offering a Steer composer that writes into nothing. */
export interface TodayAgentView {
  running: boolean;
  agent: TodayAgentRecord | null;
  health?: AgentHealth | null;
  control?: AgentControl | null;
  last_activity_ts?: number | null;
  output_tokens?: number | null;
}

// --- Triggerable-agent catalog (GET /api/agents/catalog .data; #557) ---

/** The manifest's explicit runnable agent kind, or a plain cron/native job. */
export type CatalogKind = "command" | "staff" | "hosted" | "specialist" | "cron";

/** One triggerable agent/job — `bn_core::action_center::CatalogItem` (#557). Everything the
 * automation can fire (declared crons + native jobs + the Markdown agent roster), with the
 * metadata needed to decide whether to re-trigger it. `job` is exactly what
 * `POST /api/cron/run {job}` fires;
 * specialists are install-only and are not emitted by this triggerable catalog. */
export interface CatalogItem {
  job: string;
  name: string;
  kind: CatalogKind;
  scope: string;
  declared: boolean;
  /** Neither a calendar nor startup trigger is declared. */
  on_demand: boolean;
  schedule?: string | null;
  description?: string | null;
  has_script: boolean;
  network: boolean;
  catch_up: boolean;
  startup: boolean;
  queue: boolean;
  retry_after_secs?: number | null;
  last_run?: number | null;
  last_exit?: number | null;
  ok?: boolean | null;
  runs: number;
  failures: number;
  skills?: string[];
  mcps?: string[];
  harness?: string | null;
}

/** GET /api/agents/catalog .data — the triggerable-agent catalog (`payload::catalog_data`, #557). */
export interface CatalogView {
  count: number;
  agents: CatalogItem[];
}

// --- Skills (GET /api/skills .data; GET /api/skills/:name .data; #483) ---

/** One discovered skill — `bn_core::skills::SkillInfo`. `effective` marks the highest-precedence
 * definition for its name (what bn-mcp serves); the others are shadowed. */
export interface SkillInfo {
  name: string;
  description: string;
  when?: string | null;
  keywords?: string[];
  /** Tier label: `global-shared` | `global-personal` | `repo-shared` | `repo-personal-*`. */
  scope: string;
  path: string;
  effective: boolean;
}

/** GET /api/skills .data — every skill across the tiers (`payload::skills_data`). */
export interface SkillsView {
  count: number;
  skills: SkillInfo[];
}

/** GET /api/skills/:name .data — one skill's editable source: its metadata + raw markdown. */
export interface SkillDetail {
  skill: SkillInfo;
  content: string;
}

/** The tiers a new skill can be written into, lowest → highest precedence. Mirrors
 * `bn_core::skills::SkillScope`; the repo tiers need bn-web to be running inside a git repo. */
export const SKILL_SCOPES = [
  "global-shared",
  "global-personal",
  "repo-shared",
  "repo-personal-notes",
  "repo-personal-local",
] as const;

export type SkillScope = (typeof SKILL_SCOPES)[number];

/** One harness root the sync could not refresh — e.g. a name clash with a skill bn doesn't own. */
export interface SkillNativeFailure {
  harness: string;
  /** `global` (`~/.claude`, `~/.copilot`) or `repo` (`.claude/skills`, `.github/skills`). */
  scope: string;
  error: string;
}

/** The native-harness sync every skill create/edit/delete runs (`actions::sync_native_skills_summary`).
 * Non-fatal: the bn source file is already written, so `synced: false` means some harness root is
 * stale, not that the write failed. Each root syncs independently, so the result can be partial —
 * `harnesses` lists what was refreshed, `failures` what wasn't. */
export interface SkillNativeSync {
  /** True only when every root bn tried to refresh succeeded. */
  synced: boolean;
  /** Which harnesses were actually touched — `claude`, `copilot`, or both. */
  harnesses: string[];
  created: number;
  updated: number;
  removed: number;
  unchanged: number;
  failures: SkillNativeFailure[];
}

/** The reply every skill mutation returns: the affected skill plus the harness sync result.
 * `remaining_scope` (delete only) is the tier that now defines the name — a lower-tier definition
 * the delete unshadowed — or `null` when the name is gone entirely. */
export interface SkillMutation {
  skill: SkillInfo | { name: string; scope: string; path: string };
  native: SkillNativeSync;
  remaining_scope?: string | null;
}

// --- Goals (GET /api/goals .data) ---

/** A note connected to a daily goal (#564) — `bn_core::model::GoalNote`. Either a linked branch
 * note (`repo`+`branch`, jumpable to the Notes view) and/or attached free-text / uploaded content
 * (`text`), optionally titled. */
export interface GoalNote {
  id: string;
  repo?: string | null;
  branch?: string | null;
  text?: string | null;
  title?: string | null;
  created: string;
}

/** One daily goal — `payload::daily_data` goal row. `est`/`actual` are aimed-vs-achieved minutes;
 * `pct` is partial progress on a still-open goal; `q` is the aligned quarter-goal key (#490). */
export interface DailyGoal {
  id: string;
  text: string;
  link?: string | null;
  done: boolean;
  done_at?: string | null;
  created: string;
  pct?: number | null;
  /** Human durations (`90m`, `2h`) — aimed vs achieved from `bn focus` calibration. */
  est?: string | null;
  actual?: string | null;
  /** Aligned quarter-goal key (`bn daily add --q <key>`), or null when untagged. */
  q?: string | null;
  /** Notes connected to this goal (#564) — linked branch notes and/or attached content. */
  notes?: GoalNote[];
}

/** A scheduled-but-not-yet-today goal — `payload::daily_data` upcoming row. */
export interface UpcomingGoal {
  id: string;
  text: string;
  link?: string | null;
  scheduled: string;
}

/** The daily half of GET /api/goals .data — `payload::daily_data`. */
export interface DailyGoals {
  date: string;
  open: number;
  done: number;
  goals: DailyGoal[];
  upcoming: UpcomingGoal[];
  rendered?: string;
}

/** One quarter goal — `payload::quarter_data` goal row. `status` is the derived on-track signal. */
export interface QuarterGoal {
  key: string;
  text: string;
  done: boolean;
  done_at?: string | null;
  metric?: string | null;
  status: string;
}

/** The quarter half of GET /api/goals .data — `payload::quarter_data`. */
export interface QuarterGoals {
  quarter: string | null;
  elapsed_pct?: number;
  open: number;
  done: number;
  on_track: number;
  total: number;
  goals: QuarterGoal[];
  reviews: unknown[];
  header?: string;
}

/** GET /api/goals .data — today's daily goals plus the current quarter (`api::goals`). */
export interface GoalsView {
  daily: DailyGoals;
  quarter: QuarterGoals;
}

// --- Learnings (GET /api/learnings .data; mirrors `bn_core::learnings::LearningsView`) ---

/** The overseer's review decision on a learning entry, if acted on (`learnings::Review`). */
export interface LearningReview {
  decision?: string;
  [k: string]: unknown;
}

/** One learning / reflection line — `bn_core::learnings::LearningEntry`. */
export interface LearningEntry {
  key: string;
  kind: string;
  date: string;
  text: string;
  repo?: string | null;
  branch?: string | null;
  issue?: string | null;
  harness?: string | null;
  skill?: string | null;
  review?: LearningReview | null;
  agent: boolean;
}

/** One theme group (a skill, a repo, or `general`) with its entries — `learnings::Group`. */
export interface LearningGroup {
  theme: string;
  entries: LearningEntry[];
}

/** GET /api/learnings .data — theme groups plus headline counts (`learnings::LearningsView`). */
export interface LearningsView {
  start: string;
  end: string;
  groups: LearningGroup[];
  total: number;
  learned: number;
  reflections: number;
  agent: number;
  mine: number;
  promoted: number;
  dismissed: number;
  unreviewed: number;
}

// --- Cost (GET /api/cost .data; mirrors `bn_core::cost::CostView` from #381) ---

/** The work-class category token on a cost row (`bn_core::cost::WorkClass::as_str`). Rows recorded
 * before #381 threaded a class through fold into `unattributed` (expected to dominate history). */
export type WorkClass =
  "orchestration" | "coding" | "scheduled" | "research" | "remote" | "unattributed";

/** One grouped cost line — `bn_core::cost::CostRow`. `aic` (the *metered Copilot bill*) is `null`
 * for a group with no Copilot runs; `cost_usd` (the *API-equiv benchmark*) is `null` when nothing
 * in the group had a priceable model. */
export interface CostRow {
  key: string;
  /** Fresh (non-cached) input tokens. */
  input_tokens: number;
  /** Cached prompt tokens (priced ~10× cheaper). */
  cache_read_tokens: number;
  output_tokens: number;
  sessions: number;
  /** AIC — the real Copilot bill. */
  aic: number | null;
  /** $ — the cache-aware API-equiv benchmark. */
  cost_usd: number | null;
}

/** Whole-window totals in both cost units — `bn_core::cost::CostTotals`. */
export interface CostTotals {
  sessions: number;
  input_tokens: number;
  cache_read_tokens: number;
  output_tokens: number;
  aic: number | null;
  cost_usd: number | null;
}

/** One (day × category) cell — `bn_core::cost::DayCategory`, the stacked-bar chart's source. */
export interface DayCategory {
  day: string;
  category: string;
  sessions: number;
  input_tokens: number;
  cache_read_tokens: number;
  output_tokens: number;
  aic: number | null;
  cost_usd: number | null;
}

/** GET /api/cost .data — `bn_core::cost::CostView`. Every breakdown is always present, so the page
 * switches the table dimension client-side; only the window picker changes the fetch. */
export interface CostView {
  window_days: number;
  totals: CostTotals;
  by_category: CostRow[];
  by_machine: CostRow[];
  by_repo: CostRow[];
  by_model: CostRow[];
  by_day: CostRow[];
  by_day_category: DayCategory[];
}

// --- Wrap: today's "achieved" digest (GET /api/wrap .data; #490) ---

/** A PR merged today (`reports::MergedPr`). */
export interface WrapMergedPr {
  number: number;
  title: string;
  url: string;
}

/** A commit today with no corresponding note update (`reports::UnnotedCommit`). */
export interface WrapCommit {
  hash: string;
  subject: string;
}

/** A still-open todo across active branch notes (`reports::OpenTodo`). */
export interface WrapTodo {
  repo: string;
  branch: string;
  text: string;
}

/** A daily goal carrying into tomorrow (`reports::GoalView`). */
export interface WrapCarryGoal {
  id: string;
  text: string;
  link?: string | null;
  pending_days: number;
}

/** GET /api/wrap .data — the end-of-day digest (`payload::wrap_data` = `WrapDigest` + `rendered`). */
export interface WrapView {
  date: string;
  branches_touched: string[];
  unnoted_commits: WrapCommit[];
  merged_prs: WrapMergedPr[];
  open_todos: WrapTodo[];
  carry_forward: WrapCarryGoal[];
  rendered: string;
}

// --- Insights & metrics (#507) — GET /api/metrics · /api/metrics/trend · /api/insights ·
//     /api/weekly · /api/brag. The metrics scorecard mirrors `bn_core::metrics::Scorecard`; the
//     three digests each carry a `rendered` text/markdown field the tab shows verbatim. ---

/** PR throughput block (`metrics::Metrics`) — the flow tiles read from this. */
export interface MetricsPr {
  window_days: number;
  opened: number;
  merged: number;
  closed: number;
  in_flight: number;
  completion_rate: number | null;
  cycle_median_hours: number | null;
  cycle_p90_hours: number | null;
  lead_median_hours: number | null;
  lead_p90_hours: number | null;
}

/** AI-agent session telemetry (`metrics::AgentMetrics`) folded over the window. */
export interface MetricsAgent {
  sessions: number;
  total_minutes: number;
  by_harness: [string, number][];
  by_model: [string, number][];
  cost_aiu: number;
  input_tokens: number;
  output_tokens: number;
  lines_added: number;
  lines_removed: number;
}

/** GET /api/metrics .data — the velocity + throughput scorecard (`payload::scorecard_data`). Only
 * the fields the dashboard renders are typed; the full record carries more (baselines, churn, …). */
export interface MetricsView {
  window_days: number;
  pr: MetricsPr;
  wip: number;
  open_branches: number;
  branch_age_median_days: number | null;
  branch_age_max_days: number | null;
  agent: MetricsAgent;
}

/** One recorded metrics snapshot row behind the trend chart. */
export interface MetricsTrendPoint {
  date: string;
  wip: number;
  merged: number;
  close_out_lag_median_hours: number | null;
}

/** GET /api/metrics/trend .data — the snapshot history (empty until snapshots are written). */
export interface MetricsTrendView {
  snapshots: MetricsTrendPoint[];
}

/** GET /api/insights .data — the weekly auto-retro (`payload::insights_data`); `rendered` is the
 * text digest the tab shows. The structured metrics are carried too but the tab renders the text. */
export interface InsightsView {
  rendered: string;
  [key: string]: unknown;
}

/** GET /api/weekly .data — the weekly 1-1 review digest (`payload::weekly_data`). */
export interface WeeklyView {
  rendered: string;
  [key: string]: unknown;
}

/** GET /api/brag .data — the accomplishments / impact packet (`payload::brag_data`); `rendered` is
 * the markdown the tab shows. */
export interface BragView {
  rendered: string;
  [key: string]: unknown;
}

// --- Task-first model (#993) ---------------------------------------------------------------

/** One task from the global store (`bn_core::todo`, #996). */
export interface TaskRow {
  id: string;
  status: string;
  priority: string | null;
  summary: string;
  created: string | null;
  completion: string | null;
  due: string | null;
  snooze: string | null;
  branch: string | null;
  src: string | null;
  projects: string[];
  contexts: string[];
  repo: string | null;
  day: string | null;
  est: string | null;
  doc: string | null;
  /** The SMART gate's verdict (#998): `ok` | `fail` | `stuck`, or null when never judged. */
  smart: string | null;
  stage: string | null;
  uid: string | null;
  mod: number;
}

export interface TasksView {
  today: string;
  all: boolean;
  day: string | null;
  repo: string | null;
  count: number;
  tasks: TaskRow[];
}

export interface IssueRow {
  repo: string;
  number: number;
  title: string;
  url: string;
  state: string;
  labels: string[];
  updated_at: string | null;
  assignees: string[];
  is_assigned: boolean;
}

/** One repo's issues **and how much to trust them** (#999). `stale`/`error` are not optional:
 * an empty list with an error means "we could not ask", which is a different fact from "no open
 * issues". The UI must never render the two the same way. */
export interface RepoIssues {
  repo: string;
  slug: string | null;
  fetched_at: number;
  stale: boolean;
  error: string | null;
  issues: IssueRow[];
}

export interface IssuesView {
  repos: RepoIssues[];
  count: number;
  stale_repos: number;
  unreachable_repos: number;
}

/** One node of the nine-stage pipeline drawing (#746 made visible by #1005). */
export interface PipelineStage {
  id: string;
  glyph: string;
  position: number;
  count: number;
  /** The workflow skill this stage dispatches by default — its *intent*. `null` for the stages
   * that run no harness workflow at all (pickup leases, consensus votes, validate is `bn verify`,
   * ship hands off), which is a fact rather than a missing value. */
  skill: string | null;
}

/** One worktree's row on the board. `skill` is what it is **actually** running, read from the
 * `skill:` decision its dispatch recorded — which can differ from its stage's default whenever
 * someone passed an explicit `--skill`. */
export interface PipelineBoardEntry {
  repo: string;
  branch: string;
  stage: string;
  ts: number;
  skill: string | null;
}

export interface PipelineBoardView {
  board: PipelineBoardEntry[];
  counts: Record<string, number>;
  stages: PipelineStage[];
}

/** The agent currently working a task, joined on `(repo, branch)` — `LiveAgent`'s `target` *is*
 * `repo:branch`. Attached to the task row rather than listed apart, so a task and the thing
 * working on it cannot be rendered out of sync. Absent when nothing is running. */
export interface TaskAgent {
  session_id: string;
  target: string;
  harness: string;
  health: AgentHealth;
  idle_secs: number;
}

/** `GET /api/tasks/:id` — one task with everything already joined to it, so the detail page renders
 * from a single fetch and a task can never disagree with the agent shown beside it. */
export interface TaskDetailView {
  today: string;
  task: TaskRow;
  /** The live agent on this task's worktree, or null when nothing is running it. */
  agent: TaskAgent | null;
  /** The branch note for the task's `(repo, branch)`, when it has one. */
  note: NoteDetail | null;
  /** The task's context doc (`## Done When` and the rest), when one was written. */
  doc: { path: string; body: string; version: string } | null;
}

/** A task in flight: the store row, whatever is working on it, and the workflow it is running. */
export interface StageTask extends TaskRow {
  agent?: TaskAgent;
  /** The skill this worktree is **actually** running (#1041), joined from its branch note — not
   * the stage's default. An explicit `bn dispatch --skill debug` sits in `execute` running `debug`,
   * so showing the stage default would state the wrong thing confidently. */
  skill?: string | null;
}

/** A worktree recorded at a stage with **no task behind it**. Reported rather than dropped: it is
 * exactly the gap EPIC #993 closes, and hiding it would make the pipeline read as emptier than it
 * is. `count` deliberately excludes these — they are not tasks. */
export interface StageOrphan {
  repo: string;
  branch: string;
  ts: number;
}

/** One stage of `GET /api/pipeline/stages`.
 *
 * **`count === tasks.length`, by construction on the server.** The old bug — a node reading `3`
 * over an empty list — is not fixed here so much as made unrepresentable. */
export interface PipelineStageWork {
  id: string;
  glyph: string;
  position: number;
  /** The workflow this stage dispatches by *default* — its intent, as against each row's `skill`,
   * which is what that worktree is really running. */
  skill: string | null;
  count: number;
  tasks: StageTask[];
  orphans: StageOrphan[];
}

export interface PipelineStagesView {
  today: string;
  in_flight: number;
  orphans: number;
  stages: PipelineStageWork[];
}

/** A day's finished record. `done` and `cancelled` are counted separately and must never be summed
 * into one figure: the store keeps `x <date> cancelled:1` distinct from a real completion so that
 * abandoned work cannot inflate a day's output. */
export interface DoneTasksView {
  today: string;
  day: string;
  status: "done";
  count: number;
  done: number;
  cancelled: number;
  tasks: (TaskRow & { reason: string | null })[];
}

// --- Health (GET /api/health .data) ---

/** The server's own vitals — what `/settings` shows under "Server". `machine` is the identity bn
 * files this host's work under, which is not necessarily its hostname. */
export interface HealthView {
  status: string;
  version: string;
  machine: string;
  uptime_secs: number;
  /** False when the binary was built without the UI bundle baked in (a dev-server session). */
  ui_built: boolean;
  web: {
    port: number;
    bind: string;
    fleet_poll_secs: number;
  };
}
