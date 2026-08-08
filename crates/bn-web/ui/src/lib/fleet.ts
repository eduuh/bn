// Pure fleet-view logic for the redesign (#479): fold the orchestrate runs + live fleet into one
// flat list of glanceable "entries", surface the ones that "need you" (stuck / waiting-for-review),
// and map machine states to one-line human labels. Side-effect-free + unit-tested (`fleet.test.ts`)
// so the mobile Fleet view stays a thin render of these values.

import type {
  AgentControl,
  AgentRole,
  AgentTask,
  FleetView,
  LiveAgent,
  OrchestrateView,
  PaneState,
  QueueItem,
  RunView,
} from "../api/types";
import type { AgentPlanSummary } from "../api/plan-types";
import { agentForItem, healthTone, nonOrchestratedAgents, paneTone, type ChipTone } from "./board";

/** A unified fleet card viewmodel — one running agent (an in-flight orchestrate item joined to its
 * live session, or a non-orchestrated live agent), reduced to what a glanceable card renders. */
export interface FleetEntry {
  key: string;
  repo: string;
  branch: string;
  /** `repo:branch` tmux target, for the copy-jump. */
  target: string;
  /** Live session id when known — enables the detail link (`/agents/:id`). */
  sessionId?: string;
  harness?: string;
  /** Fleet role (#500) — orchestrators pin to their own top tier; defaults to `worker`. */
  role: AgentRole;
  /** Workflow type inferred from launch metadata. */
  workType?: "setup";
  /** One-line human state ("Working", "Waiting: trust prompt", …). */
  stateLabel: string;
  /** Short status chip text. */
  statusLabel: string;
  tone: ChipTone;
  outputTokens?: number;
  /** Assistant turns so far — the "is it actually moving" counter the token total can't give you. */
  turns?: number;
  /** Tool calls so far, the other half of that signal. */
  toolCalls?: number;
  updatedAt: number;
  /** True when this agent needs the human: stuck/dead/errored, or waiting-for-plan-review. */
  needs: boolean;
  needsReason?: string;
  reviewId?: string;
  pr?: string | null;
  /** Whether bn has a write channel to this agent — gates the steer controls (see [`SteerPanel`]). */
  control?: AgentControl;
  /** The task this agent is working on, when one is bound to its worktree. */
  task?: AgentTask;
  plan?: AgentPlanSummary;
}

function workTypeFor(a?: LiveAgent): FleetEntry["workType"] {
  if (!a?.task_name) return undefined;
  return a.task_name === "repo-setup" || a.task_name.startsWith("repo-setup:")
    ? "setup"
    : undefined;
}

/** Human one-liner for a #340 pane state. */
export function paneStateLabel(state?: PaneState | null): string {
  switch (state) {
    case "working":
      return "Working";
    case "idle-clean":
      return "Idle — clean";
    case "idle-with-wip":
      return "Idle — uncommitted work";
    case "trust-menu":
      return "Waiting: trust prompt";
    case "command-approval":
      return "Waiting: command approval";
    case "question-menu":
      return "Waiting: question";
    case "api-error":
      return "API error";
    case "dns-outage":
      return "Network outage";
    case "limited":
      return "Usage limited";
    case "done":
      return "Done";
    case "dead":
      return "Dead — needs relaunch";
    default:
      return "Dispatched";
  }
}

/** Whether a pane state is one that needs the human (errored / dead). */
function stateNeedsYou(state?: PaneState | null): boolean {
  return (
    state === "api-error" ||
    state === "dns-outage" ||
    state === "dead" ||
    state === "trust-menu" ||
    state === "command-approval"
  );
}

const HEALTH_LABEL: Record<string, string> = {
  starting: "Starting…",
  active: "Active",
  idle: "Idle",
  // Just "Stuck" — the *why* is the card's needs-reason strip, and saying it twice on one card
  // reads as two problems.
  stuck: "Stuck",
};

/** An in-flight orchestrate item → a fleet entry, joined to its live agent for tokens + session. */
function entryFromItem(item: QueueItem, run: RunView, fleet?: FleetView): FleetEntry {
  const agent = agentForItem(item, run.repo, fleet);
  const target = item.tmux_window
    ? `${item.tmux_session ?? run.repo}:${item.tmux_window}`
    : (agent?.target ?? `${run.repo}:${(item.branch ?? "").replace(/\//g, "-")}`);
  const needsReview = !!item.waiting_for_review;
  const needs = needsReview || stateNeedsYou(item.last_state);
  return {
    key: `${run.repo}/${item.id}`,
    repo: run.repo,
    branch: item.branch ?? item.item,
    target,
    sessionId: agent?.session_id,
    control: agent?.control,
    // Carried from the live agent, not the queue item: the binding lives on the worktree, and the
    // queue item has no idea which task it came from.
    task: agent?.task,
    plan: agent?.plan,
    harness: item.harness ?? agent?.harness,
    // An orchestrate queue item is always a ship (worker) agent; the orchestrator is the daemon
    // driving the wave, surfaced separately as a live agent below.
    role: "worker",
    workType: workTypeFor(agent),
    stateLabel: needsReview ? "Waiting: plan review" : paneStateLabel(item.last_state),
    statusLabel: needsReview ? "review" : item.last_state ? item.last_state : "in-flight",
    tone: needsReview ? "amber" : paneTone(item.last_state),
    outputTokens: agent?.output_tokens,
    turns: agent?.turns,
    toolCalls: agent?.tool_calls,
    updatedAt: item.updated_at,
    needs,
    needsReason: needsReview
      ? "Plan awaiting approval"
      : stateNeedsYou(item.last_state)
        ? paneStateLabel(item.last_state)
        : undefined,
    reviewId: item.review_id,
    pr: item.pr,
  };
}

/** A non-orchestrated live agent → a fleet entry. */
function entryFromAgent(a: LiveAgent): FleetEntry {
  const role: AgentRole = a.role ?? "worker";
  const orchestrator = role === "orchestrator";
  return {
    key: `live/${a.session_id}`,
    repo: a.repo,
    branch: a.branch,
    target: a.target,
    sessionId: a.session_id,
    control: a.control,
    task: a.task,
    plan: a.plan,
    harness: a.harness,
    role,
    workType: workTypeFor(a),
    // An orchestrator runs the wave rather than shipping, so read its health as a fleet-tick signal.
    stateLabel: orchestrator ? orchestratorStateLabel(a) : (HEALTH_LABEL[a.health] ?? a.health),
    statusLabel: orchestrator ? "orchestrator" : a.health,
    tone: healthTone(a.health),
    outputTokens: a.output_tokens,
    turns: a.turns,
    toolCalls: a.tool_calls,
    updatedAt: a.last_activity_ts,
    needs: a.health === "stuck",
    needsReason: a.health === "stuck" ? "No progress" : undefined,
  };
}

/** The orchestrator's one-line status: its running state plus how long since its last fleet tick. */
function orchestratorStateLabel(a: LiveAgent): string {
  const running = a.health === "stuck" ? "Stalled" : "Running";
  return `${running} · last tick ${fmtIdle(a.idle_secs)} ago`;
}

/** A compact "3m"/"2h"/"45s" idle string for the orchestrator status line. */
function fmtIdle(secs: number): string {
  if (secs < 60) return `${Math.max(0, Math.round(secs))}s`;
  if (secs < 3600) return `${Math.round(secs / 60)}m`;
  return `${Math.round(secs / 3600)}h`;
}

/** The full flat fleet: every in-flight orchestrate item across runs, plus non-orchestrated live
 * agents. Needs-you entries sort first, then most-recently-updated. */
export function fleetEntries(orch?: OrchestrateView, fleet?: FleetView): FleetEntry[] {
  const runs = orch?.runs ?? [];
  const entries: FleetEntry[] = [];
  for (const run of runs) {
    for (const it of run.queue) {
      if (it.status === "dispatched") entries.push(entryFromItem(it, run, fleet));
    }
  }
  for (const a of nonOrchestratedAgents(fleet, runs)) entries.push(entryFromAgent(a));
  entries.sort((x, y) => Number(y.needs) - Number(x.needs) || y.updatedAt - x.updatedAt);
  return entries;
}

/** The subset that needs the human, most-urgent first (already needs-first from [`fleetEntries`]). */
export function needsYou(entries: FleetEntry[]): FleetEntry[] {
  return entries.filter((e) => e.needs);
}

/** The orchestrator agents — pinned as a distinct top tier in the Fleet (#500). */
export function orchestrators(entries: FleetEntry[]): FleetEntry[] {
  return entries.filter((e) => e.role === "orchestrator");
}
