// What each kind of write invalidates, named once.
//
// Call sites used to spell their own key lists inline, and the same logical event reconciled
// different things depending on which screen you happened to be on: creating a task from Home
// refreshed `tasks` + the pipeline, creating one from Today refreshed only `tasks`; launching an
// agent from the console refreshed `actions` too, launching from a repo page didn't. The screen you
// started from should never decide what ends up stale, so the groups below are keyed by the *event*
// rather than by the component that raises it.
//
// Being generous here is cheap. `invalidateQueries` only refetches queries that are currently
// mounted; anything else is merely marked stale and refetched if and when it is next shown. So a
// group can safely name every surface an event could plausibly touch.
//
// Keys match by prefix, which is why `["tasks"]` covers every `tasks(scope)` and the
// `tasks/done/:day` history in one entry, and `["pipeline"]` covers the stage drill-downs.

import { type QueryKey } from "@tanstack/react-query";
import { queryKeys } from "../api/client";

const TASKS_ALL: QueryKey = ["tasks"];
const PIPELINE_ALL: QueryKey = ["pipeline"];
// `["queue"]` covers the view, its event history and its entry lists in one entry.
const QUEUE_ALL: QueryKey = ["queue"];
const QUEUE_TIMELINE_ALL: QueryKey = ["queue-timeline"];
// The per-agent dialog read, as a prefix — an answer invalidates every agent's, not just the one
// on screen, since the same dialog can be shown from the fleet strip and the agent's own panel.
const PROMPT_ONE_ALL: QueryKey = ["agent-prompt"];
const PROMPT_FLEET: QueryKey = ["fleet-prompts"];

export const affects = {
  /** A task was created, edited, completed, cancelled or re-staged. */
  task: [TASKS_ALL, PIPELINE_ALL, queryKeys.actions, queryKeys.completed] as QueryKey[],

  /** An attention item was planned, snoozed or dismissed. */
  attention: [queryKeys.attn, queryKeys.actions] as QueryKey[],

  /** A plan-review was approved or rejected. That verdict also unblocks the agent waiting on it,
   * which is why the fleet and the run are in here and not just the review feed. */
  review: [
    queryKeys.reviews,
    queryKeys.attn,
    queryKeys.actions,
    queryKeys.orchestrate,
    queryKeys.fleet,
  ] as QueryKey[],

  /** An agent was started, queued or dispatched. */
  agentLaunch: [queryKeys.fleet, queryKeys.orchestrate, queryKeys.actions] as QueryKey[],

  /** A live agent was steered, relaunched, stopped, or had its harness switched — its own timeline
   * moves as well as its fleet row. */
  agentSteer: [queryKeys.fleet, queryKeys.activityAll] as QueryKey[],

  /** An orchestrate run's own backlog or concurrency changed (the per-repo queue on Fleet). */
  backlog: [queryKeys.orchestrate, queryKeys.fleet] as QueryKey[],

  /** The machine-wide dispatcher queue had an entry added, cancelled, reprioritized
   * or cancelled. The timeline is a different root key but the same underlying queue, so it goes
   * stale on exactly the same events. */
  queue: [QUEUE_ALL, QUEUE_TIMELINE_ALL] as QueryKey[],

  /** An agent's blocked dialog was answered. Both prompt reads are in here because answering from
   * the fleet strip and answering from the agent's own panel have to leave the *other* surface
   * correct too — and because a refused answer (`prompt_moved`) is precisely when re-reading
   * matters most, which is why this reconciles on settle rather than on success. */
  approval: [PROMPT_ONE_ALL, PROMPT_FLEET, queryKeys.fleet, queryKeys.activityAll] as QueryKey[],

  /** A cron or staff job was run, enabled or disabled. Today's two schedule strips are derived from
   * the same registry, so they go stale together. */
  schedule: [
    queryKeys.cron,
    queryKeys.catalog,
    queryKeys.todayCrons,
    queryKeys.todayAgents,
    queryKeys.actions,
  ] as QueryKey[],

  /** Today's hosted agent was started or stopped. */
  todayAgent: [queryKeys.todayAgent, queryKeys.fleet] as QueryKey[],

  /** A daily goal or quarter objective changed. */
  goals: [queryKeys.goals] as QueryKey[],

  /** A repo was registered, removed or re-scanned. */
  repos: [queryKeys.repos] as QueryKey[],

  /** Pool config or warm scheduling changed. Pool work is a queue entry first, then repo state once
   * the warm completes. */
  pool: [queryKeys.repos, QUEUE_ALL, QUEUE_TIMELINE_ALL] as QueryKey[],

  /** A skill was created or edited. Pair with `queryKeys.skill(name)` for the open editor. */
  skills: [queryKeys.skills] as QueryKey[],

  /** A branch note was written to. Pair with `queryKeys.note(repo, branch)`. */
  notes: [queryKeys.notes] as QueryKey[],
} as const;
