/**
 * The task-first model (#993) — the shapes `bn_core::task` serializes.
 *
 * Split out of `types.ts`, which is the UI's largest file and sits at the file-size ratchet's
 * ceiling. Re-exported from `types.ts`, so importing from either path resolves the same types.
 */
import type { AgentHealth, NoteDetail } from "./types";

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
