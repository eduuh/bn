// The write client for the bn-web mutation endpoints (#392 write layer). The server no longer
// gates writes on a token (#523) — a loopback bind and a trusted LAN are the trust boundary — so
// this is just a small `fetch` wrapper that unwraps the standard `Envelope` and throws a typed
// `ApiError` on any transport / command failure. Reads still go through `api/client.ts`.

import type { Counts, Envelope, SkillInfo } from "./types";
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

// --- Skill edit (#483) — write back the effective skill file ---

/** `PUT /api/skills/:name {content}` — overwrite the effective skill's markdown source. Returns the
 * updated skill metadata; a `400` (invalid content) or `404` (unknown skill) throws `ApiError`. */
export function saveSkill(name: string, content: string): Promise<{ skill: SkillInfo }> {
  return request("PUT", `/api/skills/${encodeURIComponent(name)}`, { content });
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

/** `POST /api/land {repo}` — land the repo's ready PRs (`bn wt merge --all`). */
export function apiLand(repo: string): Promise<unknown> {
  return post("/api/land", { repo });
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
export function connectGoalNote(goalId: string, body: GoalNoteConnectBody): Promise<GoalNoteResult> {
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

/** `POST /api/suggestions/:id {status}` — review one suggestion (accept / dismiss / re-open). The
 * native in-process store write; returns the id + new status. */
export function setSuggestionStatus(
  id: string,
  status: SuggestionStatus,
): Promise<{ id: string; status: SuggestionStatus }> {
  return post(`/api/suggestions/${encodeURIComponent(id)}`, { status });
}

// --- Warm-worktree pool (#497) — pre-build a repo's pool slots from the dashboard ---

/** The `POST /api/repos/:repo/pool/warm` ack: the warm was *started* (it runs long, in the
 * background), carrying the detached process id and a human message. */
export interface WarmPoolResult {
  repo: string;
  status: string;
  pid: number;
  message: string;
}

/** `POST /api/repos/:repo/pool/warm` — trigger `bn wt pool warm --repo <repo>` for a pooled repo.
 * Warming is long-running, so the server spawns it detached and returns immediately; a `400`
 * (invalid or unpooled repo) throws `ApiError`. */
export function warmRepoPool(repo: string): Promise<WarmPoolResult> {
  return post<WarmPoolResult>(`/api/repos/${encodeURIComponent(repo)}/pool/warm`);
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
