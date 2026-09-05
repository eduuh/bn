// The recurring optimistic patches, written once.
//
// Most writes in the dashboard do one of a handful of things to a list: take a row out of it
// (dismiss, ack, approve), or flip a field on one row (enable a cron, complete a task). Spelling
// those inline at every call site is how the same gesture ended up feeling different depending on
// the screen, so they live here and the call site just names one.
//
// Each helper returns an `OptimisticPatch` ready to hand to `useAppMutation`.

import { queryKeys } from "../api/client";
import { optimistic, type OptimisticPatch } from "../hooks/useAppMutation";
import type {
  ActionsView,
  AttnView,
  CronView,
  SuggestionsView,
  ReviewsView,
  ReviewStatus,
  TasksView,
  GoalsView,
} from "../api/types";

/** Take an attention row off the open list — ack, plan, snooze and dismiss all remove it. */
export function dropAttn<TVars = void>(
  id: string | ((v: TVars) => string),
): OptimisticPatch<TVars> {
  const pick = typeof id === "function" ? id : () => id;
  return optimistic<AttnView, TVars>(queryKeys.attn, (view, vars) => ({
    ...view,
    items: view.items.filter((i) => i.id !== pick(vars)),
  }));
}

/** Take a row out of the action centre, keeping its `count` honest. */
export function dropAction<TVars = void>(
  id: string | ((v: TVars) => string),
): OptimisticPatch<TVars> {
  const pick = typeof id === "function" ? id : () => id;
  return optimistic<ActionsView, TVars>(queryKeys.actions, (view, vars) => {
    const actions = view.actions.filter((a) => a.id !== pick(vars));
    return { ...view, actions, count: actions.length };
  });
}

/** Take the action-centre row that stands for a given attention item off the list. The action has
 * its own id and carries the attention id in `payload.attn_id`, so matching on `id` here would
 * silently never fire. */
export function dropActionForAttn<TVars = void>(
  attnId: string | ((v: TVars) => string),
): OptimisticPatch<TVars> {
  const pick = typeof attnId === "function" ? attnId : () => attnId;
  return optimistic<ActionsView, TVars>(queryKeys.actions, (view, vars) => {
    const actions = view.actions.filter((a) => a.payload?.attn_id !== pick(vars));
    return { ...view, actions, count: actions.length };
  });
}

/** Record a plan-review's verdict in place.
 *
 * A status flip rather than a removal, because the two surfaces that show reviews want different
 * things from the same click: the inbox keeps decided reviews on screen with their new badge, while
 * the agent-detail panel renders only the *pending* one and so disappears on its own once the
 * status moves off `pending`. Flipping satisfies both; dropping would blank the inbox row. */
export function setReviewStatus<TVars = void>(
  id: string | ((v: TVars) => string),
  status: ReviewStatus,
): OptimisticPatch<TVars> {
  const pick = typeof id === "function" ? id : () => id;
  return optimistic<ReviewsView, TVars>(queryKeys.reviews, (view, vars) => ({
    ...view,
    reviews: view.reviews.map((r) => (r.id === pick(vars) ? { ...r, status } : r)),
  }));
}

/** Flip a cron job's `enabled` flag in place — the toggle should move under the cursor, not a
 * second later when the registry comes back. */
export function toggleCron<TVars = void>(
  job: string | ((v: TVars) => string),
  enabled: boolean | ((v: TVars) => boolean),
): OptimisticPatch<TVars> {
  const pickJob = typeof job === "function" ? job : () => job;
  const pickOn = typeof enabled === "function" ? enabled : () => enabled;
  return optimistic<CronView, TVars>(queryKeys.cron, (view, vars) => ({
    ...view,
    jobs: view.jobs.map((j) => (j.job === pickJob(vars) ? { ...j, enabled: pickOn(vars) } : j)),
  }));
}

/** Patch one task row in a given scope's list. */
export function patchTask<TVars = void>(
  scope: string,
  id: string | ((v: TVars) => string),
  fields: Partial<TasksView["tasks"][number]> | ((v: TVars) => Partial<TasksView["tasks"][number]>),
): OptimisticPatch<TVars> {
  const pick = typeof id === "function" ? id : () => id;
  const pickFields = typeof fields === "function" ? fields : () => fields;
  return optimistic<TasksView, TVars>(queryKeys.tasks(scope), (view, vars) => ({
    ...view,
    tasks: view.tasks.map((t) => (t.id === pick(vars) ? { ...t, ...pickFields(vars) } : t)),
  }));
}

/** Drop a task row out of a scope's list — completing or cancelling it from a filtered view. */
export function dropTask<TVars = void>(
  scope: string,
  id: string | ((v: TVars) => string),
): OptimisticPatch<TVars> {
  const pick = typeof id === "function" ? id : () => id;
  return optimistic<TasksView, TVars>(queryKeys.tasks(scope), (view, vars) => {
    const tasks = view.tasks.filter((t) => t.id !== pick(vars));
    return { ...view, tasks, count: tasks.length };
  });
}

/** Take a suggestion off the open list — accepting or dismissing both move it out of `suggested`. */
export function dropSuggestion<TVars = void>(
  id: string | ((v: TVars) => string),
): OptimisticPatch<TVars> {
  const pick = typeof id === "function" ? id : () => id;
  return optimistic<SuggestionsView, TVars>(queryKeys.suggestions("suggested"), (view, vars) => {
    const suggestions = view.suggestions.filter((s) => s.id !== pick(vars));
    return { ...view, suggestions, count: suggestions.length };
  });
}

/** Replace the daily goals block (the goals editor writes the whole list at once). */
export function setDailyGoals<TVars = void>(
  next: (current: GoalsView["daily"], vars: TVars) => GoalsView["daily"],
): OptimisticPatch<TVars> {
  return optimistic<GoalsView, TVars>(queryKeys.goals, (view, vars) => ({
    ...view,
    daily: next(view.daily, vars),
  }));
}
