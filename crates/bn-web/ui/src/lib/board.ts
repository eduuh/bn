// Pure board logic: joining the orchestrate queue to the live fleet, deriving jump targets,
// pane-state colouring, throughput/ETA, and the throughput sparkline series. All side-effect-free
// and unit-tested (`board.test.ts`) so the visual layer stays a thin render of these values.

import type { FleetView, LiveAgent, PaneState, QueueItem, RunView } from "../api/types";

/** tmux window names replace `/` with `-` (worktree layout), so a `dispatch/352` branch → window
 * `dispatch-352`. Used to reconstruct a `repo:window` target when the payload lacks tmux fields. */
export function sanitizeBranch(branch: string): string {
  return branch.replace(/\//g, "-");
}

/** The tmux `session:window` to jump to for an item: prefer #372's explicit fields, then the
 * joined live agent's `target`, then reconstruct `repo:sanitized-branch`. `null` if unknowable. */
export function tmuxTarget(
  item: QueueItem,
  repo: string,
  agent?: LiveAgent,
): string | null {
  if (item.tmux_session && item.tmux_window) {
    return `${item.tmux_session}:${item.tmux_window}`;
  }
  if (agent) return agent.target;
  if (item.branch) return `${repo}:${sanitizeBranch(item.branch)}`;
  return null;
}

/** The copyable jump command shown on a card (#347's web analog). */
export function jumpCommand(target: string): string {
  return `tmux switch-client -t ${target}`;
}

/** Join a queue item to its live agent by (repo, branch) so a card can show harness + live tokens.
 * `LiveAgent.branch` is the note-key form (slashes → dashes) while a dispatched `QueueItem.branch`
 * keeps slashes, so both sides are normalized with [`sanitizeBranch`] before comparing. */
export function agentForItem(
  item: QueueItem,
  repo: string,
  fleet?: FleetView,
): LiveAgent | undefined {
  if (!fleet || !item.branch) return undefined;
  const branch = sanitizeBranch(item.branch);
  return fleet.agents.find(
    (a) =>
      sanitizeBranch(a.branch) === branch &&
      a.repo.toLowerCase() === repo.toLowerCase(),
  );
}

/** The chip colour tone for a pane state (design §4(a)): working green, api/dns/dead red,
 * idle-with-wip / menus amber, done / idle-clean grey, anything else neutral. */
export type ChipTone = "green" | "red" | "amber" | "grey" | "neutral";
export function paneTone(state?: PaneState | null): ChipTone {
  switch (state) {
    case "working":
      return "green";
    case "api-error":
    case "dns-outage":
    case "dead":
      return "red";
    case "idle-with-wip":
    case "trust-menu":
    case "command-approval":
    case "question-menu":
    case "limited":
      return "amber";
    case "done":
    case "idle-clean":
      return "grey";
    default:
      return "neutral";
  }
}

/** Health tone for a non-orchestrated fleet agent card. */
export function healthTone(health: string): ChipTone {
  switch (health) {
    case "active":
      return "green";
    case "idle":
      return "amber";
    case "stuck":
      return "red";
    default:
      return "neutral";
  }
}

/** A run's queue split into the four board columns by status. */
export interface Columns {
  queued: QueueItem[];
  inFlight: QueueItem[];
  merged: QueueItem[];
  failed: QueueItem[];
}
export function columns(run: RunView): Columns {
  const c: Columns = { queued: [], inFlight: [], merged: [], failed: [] };
  for (const it of run.queue) {
    if (it.status === "queued") c.queued.push(it);
    else if (it.status === "dispatched") c.inFlight.push(it);
    else if (it.status === "merged") c.merged.push(it);
    else if (it.status === "failed") c.failed.push(it);
  }
  return c;
}

/** Throughput (merges/hr from merged items' `updated_at`) and a rough ETA for the queued backlog
 * (`queued ÷ throughput`, in hours). `etaHours` is `0` when nothing is queued, `null` when no
 * merges exist yet to estimate a rate. */
export interface Throughput {
  mergedPerHour: number;
  etaHours: number | null;
}
export function throughput(run: RunView, now: number): Throughput {
  const merged = run.queue.filter((i) => i.status === "merged" && i.updated_at > 0);
  const count = merged.length;
  const queued = run.counts.queued;
  if (count === 0) return { mergedPerHour: 0, etaHours: null };

  const startSecs =
    run.started_at > 0
      ? run.started_at
      : Math.min(...merged.map((m) => m.updated_at));
  const elapsedHours = Math.max((now - startSecs) / 3600, 1 / 60);
  const rate = count / elapsedHours;
  const etaHours = queued === 0 ? 0 : rate > 0 ? queued / rate : null;
  return { mergedPerHour: rate, etaHours };
}

/** Hourly merge-count buckets covering `[now - hours, now]`, for the throughput sparkline.
 * Returns aligned `xs` (bucket-start epoch secs) and `ys` (merges in that hour). */
export function mergeSparkline(
  run: RunView,
  now: number,
  hours = 12,
): { xs: number[]; ys: number[] } {
  const nowHour = Math.floor(now / 3600);
  const xs: number[] = [];
  const ys: number[] = [];
  for (let h = hours - 1; h >= 0; h--) {
    xs.push((nowHour - h) * 3600);
    ys.push(0);
  }
  for (const it of run.queue) {
    if (it.status !== "merged" || it.updated_at <= 0) continue;
    const bucketHour = Math.floor(it.updated_at / 3600);
    const idx = hours - 1 - (nowHour - bucketHour);
    if (idx >= 0 && idx < ys.length) ys[idx] += 1;
  }
  return { xs, ys };
}

/** Agents not owned by any run's queue (matched by repo + normalized branch) — the non-orchestrated
 * fleet rail (`bn switch`, staff runs). Branches are [`sanitizeBranch`]-normalized because the live
 * fleet reports the dash form while the queue keeps slashes. */
export function nonOrchestratedAgents(
  fleet: FleetView | undefined,
  runs: RunView[],
): LiveAgent[] {
  if (!fleet) return [];
  const tracked = new Set<string>();
  for (const run of runs) {
    for (const it of run.queue) {
      if (it.branch) {
        tracked.add(`${run.repo.toLowerCase()}/${sanitizeBranch(it.branch)}`);
      }
    }
  }
  return fleet.agents.filter(
    (a) => !tracked.has(`${a.repo.toLowerCase()}/${sanitizeBranch(a.branch)}`),
  );
}

/** A GitHub href for a PR field when it is already a URL; `null` for a bare number (no owner known
 * from the payload), which the card renders as plain `#N` text. */
export function prHref(pr?: string | null): string | null {
  if (!pr) return null;
  return /^https?:\/\//.test(pr) ? pr : null;
}

/** The short PR label: `#352` for a number, or the trailing `#N` of a PR URL, else the raw value. */
export function prLabel(pr: string): string {
  const m = pr.match(/\/pull\/(\d+)/);
  if (m) return `#${m[1]}`;
  const bare = pr.replace(/^#/, "");
  if (/^\d+$/.test(bare)) return `#${bare}`;
  return pr;
}
