// The write client for the bn-web mutation endpoints (#392 write layer). The server no longer
// gates writes on a token (#523) — a loopback bind and a trusted LAN are the trust boundary — so
// this is just a small `fetch` wrapper that unwraps the standard `Envelope` and throws a typed
// `ApiError` on any transport / command failure. Reads still go through `api/client.ts`.

import type { Counts, Envelope, SkillMutation, SkillScope } from "./types";
import type { HandoffView } from "./types";
import type { GoalNote } from "./types";

/** A failed write. `status` is the HTTP status (`0` for a transport error); `code` is the bn
 * envelope's machine error code when present. */
export class ApiError extends Error {
  readonly status: number;
  readonly code?: string;
  constructor(message: string, status: number, code?: string) {
    super(message);
    this.name = "ApiError";
    this.status = status;
    this.code = code;
  }
}

/** Issue a JSON request and return the envelope's `data`, throwing `ApiError` on a transport
 * failure, a non-2xx status, or a command that returned `ok:false` (process writes reply `200`
 * with a failure envelope — surfacing it as an error drives the page's toast). */
async function request<T>(method: string, url: string, body?: unknown): Promise<T> {
  const headers: Record<string, string> = {};
  let payload: string | undefined;
  if (body !== undefined) {
    headers["Content-Type"] = "application/json";
    payload = JSON.stringify(body);
  }

  let res: Response;
  try {
    res = await fetch(url, { method, headers, body: payload });
  } catch (e) {
    throw new ApiError(e instanceof Error ? e.message : "network error", 0);
  }

  let env: Envelope<T> | null = null;
  try {
    env = (await res.json()) as Envelope<T>;
  } catch {
    /* a body-less / torn response falls through to the status-based error below */
  }

  if (!res.ok || !env || !env.ok) {
    const msg = env?.error?.message ?? `${method} ${url} → HTTP ${res.status}`;
    throw new ApiError(msg, res.status, env?.error?.code ?? undefined);
  }
  return env.data;
}

const post = <T>(url: string, body?: unknown) => request<T>("POST", url, body);
const del = <T>(url: string) => request<T>("DELETE", url);

// --- Endpoint wrappers (bodies mirror `bn_web::actions` exactly) ---

export interface QueueAddResult {
  repo: string;
  added: string[];
  counts: Counts;
}
export interface RemoveResult {
  repo: string;
  id: string;
  removed: boolean;
  counts: Counts;
}
export interface ReorderResult {
  repo: string;
  changed: boolean;
  counts: Counts;
}
export interface ScaleResult {
  repo: string;
  concurrency: number;
  effective: number;
}

/** `POST /api/orchestrate/queue` — append queued items to a live run. */
export function apiQueueAdd(body: { repo?: string; items: string[] }) {
  return post<QueueAddResult>("/api/orchestrate/queue", body);
}

/** `DELETE /api/orchestrate/queue/:id` — drop a still-queued item. */
export function apiQueueRemove(id: string, repo?: string) {
  const q = repo ? `?repo=${encodeURIComponent(repo)}` : "";
  return del<RemoveResult>(`/api/orchestrate/queue/${encodeURIComponent(id)}${q}`);
}

/** `POST /api/orchestrate/queue/reorder` — permute the queued subset to follow `ids`. */
export function apiQueueReorder(body: { repo?: string; ids: string[] }) {
  return post<ReorderResult>("/api/orchestrate/queue/reorder", body);
}

/** `POST /api/orchestrate/scale` — set the run's concurrency cap live. */
export function apiScale(body: { repo?: string; concurrency: number }) {
  return post<ScaleResult>("/api/orchestrate/scale", body);
}

/** `POST /api/orchestrate/stop` — stop the daemon (native flag + launchd unload). */
export function apiStop(body: { repo?: string }) {
  return post<unknown>("/api/orchestrate/stop", body);
}

/** `POST /api/orchestrate/start` — start / extend a launchd-daemonized run. */
export function apiStart(body: { repo?: string; items: string[]; concurrency?: number }) {
  return post<unknown>("/api/orchestrate/start", body);
}

/** `POST /api/dispatch` — a one-off agent, skipping the queue. */
export function apiDispatch(body: { repo: string; item: string; harness: string; kind: string }) {
  return post<unknown>("/api/dispatch", body);
}

// --- Attention actions (#483) — resolve an attention item from the dashboard ---

/** The verbs `POST /api/attention/:id` accepts (`bn_web::actions::attn_action`). */
export type AttnAction = "ack" | "remove" | "plan" | "snooze";

/** `POST /api/attention/:id {action, section?, until?}` — resolve an attention item. `plan` may take
 * a branch-note `section` (default `todo`); `snooze` needs an `until` (`YYYY-MM-DD`) date. */
export function attnAction(
  id: string,
  action: AttnAction,
  opts?: { section?: string; until?: string },
): Promise<unknown> {
  return post(`/api/attention/${encodeURIComponent(id)}`, { action, ...opts });
}

// --- Handoff write (#509) — persist a branch's resume packet from the dashboard ---

/** The author-supplied layers `POST /api/handoff` accepts (`bn_web::actions::HandoffWriteBody`). */
export interface HandoffWriteBody {
  repo: string;
  branch: string;
  narrative?: string;
  deferred?: string[];
  questions?: string[];
  priority_queue?: string[];
}

/** `POST /api/handoff {repo, branch, narrative?, …}` — compose + persist the branch's resume packet
 * (`handoff.json` + the `.md` mirror). Open per #526; returns the written packet + rendered markdown. */
export function saveHandoff(body: HandoffWriteBody): Promise<HandoffView> {
  return post<HandoffView>("/api/handoff", body);
}

// --- Skill CRUD (#483) — the skill source file, plus a Claude/Copilot native sync per write ---

/** The body `POST /api/skills` accepts (`bn_web::actions::SkillCreateBody`). `scope` defaults to
 * `global-personal` server-side; `overwrite` replaces a same-named skill in that tier. */
export interface SkillCreateBody {
  name: string;
  description: string;
  scope?: SkillScope;
  body?: string;
  overwrite?: boolean;
}

/** `POST /api/skills {name, description, scope?, body?}` — create a skill and materialize it into
 * the native Claude/Copilot skill roots. A `400` (bad name/description/scope) or `409` (that tier
 * already defines the name) throws `ApiError`. */
export function createSkill(body: SkillCreateBody): Promise<SkillMutation> {
  return post<SkillMutation>("/api/skills", body);
}

/** `PUT /api/skills/:name {content}` — overwrite the effective skill's markdown source, then re-sync
 * the native harnesses. A `400` (invalid content) or `404` (unknown skill) throws `ApiError`. */
export function saveSkill(name: string, content: string): Promise<SkillMutation> {
  return request("PUT", `/api/skills/${encodeURIComponent(name)}`, { content });
}

/** `DELETE /api/skills/:name` — delete the effective definition and sweep it out of the native
 * harness roots. A lower tier defining the same name is unshadowed, not removed — the reply's
 * `remaining_scope` says which. A `404` (unknown skill) throws `ApiError`. */
export function deleteSkill(name: string): Promise<SkillMutation> {
  return del<SkillMutation>(`/api/skills/${encodeURIComponent(name)}`);
}

// --- Agent steer actions (#394, design §4(c)) — the per-agent write endpoints ---

/** `POST /api/agents/:target/send {message}` — steer a running agent with a message. */
export function sendMessage(target: string, message: string): Promise<unknown> {
  return post(`/api/agents/${encodeURIComponent(target)}/send`, { message });
}

/** `POST /api/agents/:target/relaunch {hard}` — recover/relaunch an interrupted agent. */
export function relaunchAgent(target: string, hard: boolean): Promise<unknown> {
  return post(`/api/agents/${encodeURIComponent(target)}/relaunch`, { hard });
}

/** `POST /api/agents/:target/relaunch {harness}` (#501) — relaunch the agent in place under a
 * different harness (same worktree/branch, note re-seeded). The server availability-gates the
 * harness (a disabled one is a `400`), so this throws `ApiError` when it isn't usable. */
export function switchHarness(target: string, harness: string): Promise<unknown> {
  return post(`/api/agents/${encodeURIComponent(target)}/relaunch`, { harness });
}

/** `POST /api/agents/:target/kill` — close the agent's tmux window (frees the slot). */
export function killAgent(target: string): Promise<unknown> {
  return post(`/api/agents/${encodeURIComponent(target)}/kill`);
}

// --- Goal notes (#564) — connect/disconnect a note on a daily goal ---

/** The reply shape both goal-note writes return: the goal the note now hangs on + the note. */
export interface GoalNoteResult {
  goal: { id: string; text: string };
  note: GoalNote;
}

/** The body `POST /api/goals/:id/notes` accepts (`bn_web::actions::GoalNoteConnectBody`): a linked
 * branch note (`repo`+`branch`) and/or attached free-text / uploaded content (`text`), optionally
 * titled. At least one of a complete link or `text` is required. */
export interface GoalNoteConnectBody {
  repo?: string;
  branch?: string;
  text?: string;
  title?: string;
}

/** `POST /api/goals/:id/notes {repo?, branch?, text?, title?}` — connect a note to today's daily
 * goal `id` (#564). Open per #526; a `400` (empty/invalid) or `404` (no such goal) throws. */
export function connectGoalNote(
  goalId: string,
  body: GoalNoteConnectBody,
): Promise<GoalNoteResult> {
  return post<GoalNoteResult>(`/api/goals/${encodeURIComponent(goalId)}/notes`, body);
}

/** `DELETE /api/goals/:id/notes/:noteId` — disconnect a connected note from a daily goal (#564). */
export function disconnectGoalNote(goalId: string, noteId: string): Promise<GoalNoteResult> {
  return del<GoalNoteResult>(
    `/api/goals/${encodeURIComponent(goalId)}/notes/${encodeURIComponent(noteId)}`,
  );
}

// --- Cron re-run (#538) — fire a job on demand from the dashboard ---
/** `POST /api/cron/run {job}` — run a cron job now (the same `bn run <job>` code path a timer
 * fires). Returns the runner's synthesized envelope (exit + captured output tail); a non-zero exit
 * surfaces as an `ApiError` so the page toasts the failure. */
export function runCronJob(job: string): Promise<unknown> {
  return post("/api/cron/run", { job });
}

// --- Today's agent (#577) — synthesize cron findings into suggested tasks + review them ---

import type { SuggestionStatus, TodayRunResult } from "./types";

/** `POST /api/today/run {lookback_days?, per_source?}` — run Today's agent now: gather the cron
 * findings, synthesize suggested tasks, and merge them into the store idempotently. Returns the
 * run report plus the refreshed open suggestions so the page can update without a second fetch. */
export function runTodayAgent(body?: {
  lookback_days?: number;
  per_source?: number;
}): Promise<TodayRunResult> {
  return post<TodayRunResult>("/api/today/run", body ?? {});
}

/** `POST /api/today/agent/start {harness?}` — launch Today's **hosted** agent into the tmux
 * `planning` session. Idempotent server-side: a live agent for today is reported, not duplicated,
 * so a double-click cannot fork the day. Returns the runner's envelope. */
export function startTodayAgent(body?: { harness?: string }): Promise<unknown> {
  return post<unknown>("/api/today/agent/start", body ?? {});
}

/** `POST /api/today/agent/stop` — close Today's agent's window and release its lease. Idempotent:
 * stopping one that is already gone succeeds. */
export function stopTodayAgent(): Promise<unknown> {
  return post<unknown>("/api/today/agent/stop", {});
}

/** `POST /api/suggestions/:id {status}` — review one suggestion (accept / dismiss / re-open). The
 * native in-process store write; returns the id + new status. */
export function setSuggestionStatus(
  id: string,
  status: SuggestionStatus,
): Promise<{ id: string; status: SuggestionStatus }> {
  return post(`/api/suggestions/${encodeURIComponent(id)}`, { status });
}

// --- Warm-worktree pool (#497) — schedule repo pool builds from the dashboard ---

/** The `POST /api/repos/:repo/pool/warm` ack: the warm was queued as a heavy `pool-warm` job. */
export interface WarmPoolResult {
  added: number;
  entries: unknown[];
}

/** `POST /api/repos/:repo/pool/warm` — enqueue `pool-warm` for a pooled repo. Warming is
 * long-running, so the machine-wide queue admits it as heavy work instead of the server spawning an
 * uncapped detached build; a `400` (invalid or unpooled repo) throws `ApiError`. */
export function warmRepoPool(repo: string): Promise<WarmPoolResult> {
  return post<WarmPoolResult>(`/api/repos/${encodeURIComponent(repo)}/pool/warm`);
}

/** `POST /api/repos/:repo/pool {size?}` — pool a repo at `size` slots, or stop pooling it with
 * `size: null`. Enabling/resizing writes the pools config and queues `pool-warm` so new slots are
 * built under queue capacity rather than on the request path. */
export function setRepoPool(
  repo: string,
  size: number | null,
): Promise<{ repo: string; size: number | null; message: string }> {
  return post(`/api/repos/${encodeURIComponent(repo)}/pool`, { size });
}

/** `POST /api/repos/:repo/orchestrate {goal?, harness?}` (#556) — start a guarded main-branch
 * orchestrator for the repo: the long-lived supervisor that drains the repo's open-issue backlog.
 * Idempotent — a `409` (an orchestrator already runs for this repo) throws `ApiError` with code
 * `already_running`. */
export function startRepoOrchestrator(
  repo: string,
  body?: { goal?: string; harness?: string },
): Promise<unknown> {
  return post(`/api/repos/${encodeURIComponent(repo)}/orchestrate`, body ?? {});
}

/** `POST /api/repos/:repo/setup-agent {harness?}` — start a guarded Fleet-visible repo setup
 * agent that writes/updates bn setup scripts for the repo. */
export function startRepoSetupAgent(repo: string, body?: { harness?: string }): Promise<unknown> {
  return post(`/api/repos/${encodeURIComponent(repo)}/setup-agent`, body ?? {});
}

/** `POST /api/repos/setup-agents {harness?}` — scan for repos that need setup and start one
 * Fleet-visible setup agent for each, skipping repos that already have one live. */
export function startNeededRepoSetupAgents(body?: { harness?: string }): Promise<{
  started: string[];
  skipped: { repo: string; reason: string }[];
  failed: unknown[];
}> {
  return post("/api/repos/setup-agents", body ?? {});
}

/** `PUT /api/repos/:repo/scripts/:name {content}` — save a repo-main job script. */
export function saveRepoScript(
  repo: string,
  name: string,
  content: string,
): Promise<{ repo: string; name: string; path: string }> {
  return request(
    "PUT",
    `/api/repos/${encodeURIComponent(repo)}/scripts/${encodeURIComponent(name)}`,
    { content },
  );
}

// --- Plan-review gate (#349) — the human's decision on a paused agent's plan ---

import type { Review } from "./types";

/** The decision verbs `POST /api/reviews/:id` accepts (`bn_core::plan_review::Decision`). */
export type ReviewAction = "approve" | "request_changes" | "reject";

/** `POST /api/reviews/:id {action, notes?}` — approve / request-changes / reject a plan-review.
 * `notes` is required for `request_changes` (sent back to the agent). Returns the updated record. */
export function decideReview(
  id: string,
  action: ReviewAction,
  notes?: string,
): Promise<{ review: Review }> {
  return post(`/api/reviews/${encodeURIComponent(id)}`, { action, notes });
}

/** `POST /api/notes/:repo/:branch/plan/promote {draft}` — make a draft the active plan. */
export function promotePlanDraft(repo: string, branch: string, draft: string): Promise<unknown> {
  return post(`/api/notes/${encodeURIComponent(repo)}/${encodeURIComponent(branch)}/plan/promote`, {
    draft,
  });
}

// --- Task writes (#1006), split by latency ---------------------------------------------------
//
// `createTask` / `taskAction` are file writes: they answer with the result. `taskAgent` launches
// an LLM agent that runs for minutes, so the server spawns it detached and answers immediately —
// the caller watches the fleet rather than holding a request open.

/** A URL recorded with a task — where the work came from. */
export interface TaskLink {
  url: string;
  title?: string;
}

/** One file recorded with a task. `data` is base64, with or without the `data:` prefix a
 * `FileReader` produces — the server strips either. */
export interface TaskAttachmentUpload {
  name: string;
  data: string;
}

export interface TaskCreate {
  title: string;
  repo?: string;
  day?: string;
  est?: string;
  priority?: string;
  /** Markdown prose captured with the task. */
  body?: string;
  links?: TaskLink[];
  attachments?: TaskAttachmentUpload[];
  /** Provenance — an issue URL, say. */
  src?: string;
}

/** What a create actually recorded.
 *
 * `warnings` is non-empty when the task landed but some of its context did not — the store write
 * is deliberately the only part that can fail the whole call, so this is how the operator learns
 * that a screenshot was rejected without also losing the task. */
export interface TaskCreated {
  id: string;
  attachments: string[];
  warnings: string[];
}

/** `POST /api/tasks` — create a task and everything captured with it, in one request. */
export function createTask(body: TaskCreate): Promise<TaskCreated> {
  return post("/api/tasks", body);
}

/** One stored attachment as `GET /api/tasks/:id/attach` reports it. */
export interface TaskAttachment {
  name: string;
  size: number;
  human_size: string;
  mime: string;
  is_image: boolean;
}

/** `GET /api/tasks/:id/attach` — what is attached, without the bytes. */
export function taskAttachments(
  id: string,
): Promise<{ id: string; count: number; attachments: TaskAttachment[] }> {
  return request("GET", `/api/tasks/${encodeURIComponent(id)}/attach`);
}

/** `POST /api/tasks/:id/attach` — add files to a task that already exists. */
export function addTaskAttachments(
  id: string,
  attachments: TaskAttachmentUpload[],
): Promise<{ id: string; attachments: string[] }> {
  return post(`/api/tasks/${encodeURIComponent(id)}/attach`, { attachments });
}

/** `DELETE /api/tasks/:id/attach/:name` — drop one file and re-sync the doc's manifest. */
export function deleteTaskAttachment(
  id: string,
  name: string,
): Promise<{ id: string; removed: boolean }> {
  return del(`/api/tasks/${encodeURIComponent(id)}/attach/${encodeURIComponent(name)}`);
}

/** The URL that serves one attachment's bytes — for an `<img src>` or a download link. */
export function taskAttachmentUrl(id: string, name: string): string {
  return `/api/tasks/${encodeURIComponent(id)}/attach/${encodeURIComponent(name)}`;
}

/** The fast mutations `POST /api/tasks/:id` accepts. */
export type TaskAction = "day" | "priority" | "done" | "wip" | "snooze" | "cancel";

/** `POST /api/tasks/:id {action, value?}` — schedule, prioritize, finish, start, defer or cancel.
 *
 * `wip` is gated: the server answers **409 `not_triaged`** for a task that has not been triaged,
 * so hiding the Start button is a courtesy to the operator, not the enforcement. Callers must
 * handle that rejection rather than assume a disabled button prevented it.
 *
 * `cancel` takes the reason as `value`. It is the delete: the store never purges, and `cancelled:1`
 * is what keeps abandoned work from ever being counted as a completion. */
export function taskAction(
  id: string,
  action: TaskAction,
  value?: string,
): Promise<{ id: string; action: string }> {
  return post(`/api/tasks/${encodeURIComponent(id)}`, { action, value });
}

/** `POST /api/tasks/:id/agent {agent, steer?}` — launch the enhance or triage agent.
 *
 * Returns as soon as the process is spawned, **not** when the agent finishes: these take minutes,
 * and holding the request open would tie up a server worker with no progress and no way to cancel.
 * Watch `/api/fleet` for what happens next. */
export function taskAgent(
  id: string,
  agent: "enhance" | "improve" | "triage",
  steer?: string,
): Promise<{ id: string; agent: string; started: boolean; watch: string }> {
  return post(`/api/tasks/${encodeURIComponent(id)}/agent`, { agent, steer });
}

/** The task doc after a write: its body and the new version tag to guard the next edit with. */
export interface TaskDocResult {
  path: string;
  body: string;
  version: string;
}

/** `POST /api/tasks/:id/doc/append {text, section?}` — append context under a section (default
 * `Investigation`). Additive and unguarded: two appends both land, so this never conflicts. */
export function appendTaskDoc(id: string, text: string, section?: string): Promise<TaskDocResult> {
  return post(`/api/tasks/${encodeURIComponent(id)}/doc/append`, { text, section });
}

/** `PUT /api/tasks/:id/doc {body, if_match}` — replace the whole doc, guarded by the version it was
 * read at. A stale version throws `ApiError` with code **`doc_conflict`** (HTTP 409) — the write is
 * refused, not lost, so the caller keeps the operator's text and reloads to show what landed. */
export function editTaskDoc(id: string, body: string, ifMatch: string): Promise<TaskDocResult> {
  return request("PUT", `/api/tasks/${encodeURIComponent(id)}/doc`, { body, if_match: ifMatch });
}

/** Enqueue work. Priority is first-class rather than advanced: "run this next" is the request
 * people actually have, and without it the only way to jump the order was to re-add the item and
 * know that a re-add raises. */
export function queueAdd(body: {
  item: string;
  repo?: string;
  kind?: string;
  priority?: number;
  job?: string;
  skill?: string;
}): Promise<unknown> {
  return request("POST", "/api/queue/add", body);
}

/** Take pending work back out. Pending only — a leased entry has an executor running. */
export function queueCancel(repo: string, id: string): Promise<unknown> {
  return request("POST", "/api/queue/cancel", { repo, id });
}

/** Move queued work up or down the order — sets the priority exactly, in either direction. */
export function queuePriority(repo: string, id: string, priority: number): Promise<unknown> {
  return request("POST", "/api/queue/priority", { repo, id, priority });
}

export function queueSnooze(repo: string, id: string, forDuration: string): Promise<unknown> {
  return request("POST", "/api/queue/snooze", { repo, id, for: forDuration });
}

export function queueClass(repo: string, id: string, klass: string): Promise<unknown> {
  return request("POST", "/api/queue/class", { repo, id, class: klass });
}
