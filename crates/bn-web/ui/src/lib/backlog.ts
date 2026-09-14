// Pure backlog logic: parsing the add-form textarea into items, isolating the queued subset, and
// the optimistic cache patches (queue reorder / append / remove and per-run field edits). All
// side-effect-free and unit-tested (`backlog.test.ts`) so the components stay a thin render — the
// SSE `orchestrate` frame remains the source of truth and reconciles these local patches.

import type { OrchestrateView, QueueItem, RunView } from "../api/types";

/** Split the add-form textarea into items: one per line, trimmed, empties dropped, de-duplicated
 * (order-preserving). Matches the backend's own trim/empty filter so what the user sees queued is
 * what the server accepts. */
export function parseItems(text: string): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  for (const raw of text.split(/\r?\n/)) {
    const s = raw.trim();
    if (!s || seen.has(s)) continue;
    seen.add(s);
    out.push(s);
  }
  return out;
}

/** The still-queued items of a run, in queue order (the reorderable / removable subset). */
export function queuedItems(run: RunView): QueueItem[] {
  return run.queue.filter((i) => i.status === "queued");
}

/**
 * Permute a run's queue so its queued items follow `orderedIds` (unlisted queued items keep their
 * order after the listed ones), leaving every non-queued item in its original slot. Mirrors the
 * backend `reorder_queued` so the optimistic patch matches the frame that reconciles it.
 */
export function applyQueuedOrder(queue: QueueItem[], orderedIds: string[]): QueueItem[] {
  const queued = queue.filter((i) => i.status === "queued");
  const byId = new Map(queued.map((i) => [i.id, i] as const));
  const next: QueueItem[] = [];
  for (const id of orderedIds) {
    const it = byId.get(id);
    if (it) {
      next.push(it);
      byId.delete(id);
    }
  }
  for (const it of queued) if (byId.has(it.id)) next.push(it);
  let qi = 0;
  return queue.map((i) => (i.status === "queued" ? next[qi++] : i));
}

/** Move the id at `from` to index `to` (a drag drop), returning a new array; a no-op for an
 * out-of-range or same-slot move. */
export function moveId(ids: string[], from: number, to: number): string[] {
  if (from === to || from < 0 || to < 0 || from >= ids.length || to >= ids.length) return ids;
  const next = ids.slice();
  const [x] = next.splice(from, 1);
  next.splice(to, 0, x);
  return next;
}

/** Replace the run matching `repo` with `fn(run)`, returning a new view (unmatched runs untouched).
 * The single choke point every optimistic queue/lane patch flows through. */
export function patchRun(
  view: OrchestrateView,
  repo: string,
  fn: (run: RunView) => RunView,
): OrchestrateView {
  return { ...view, runs: view.runs.map((r) => (r.repo === repo ? fn(r) : r)) };
}

/** Recount a run's `counts` from its queue after an optimistic add/remove so the header stats stay
 * consistent until the frame lands. */
export function recount(run: RunView): RunView {
  const counts = { queued: 0, in_flight: 0, merged: 0, failed: 0, total: run.queue.length };
  for (const it of run.queue) {
    if (it.status === "queued") counts.queued++;
    else if (it.status === "dispatched") counts.in_flight++;
    else if (it.status === "merged") counts.merged++;
    else if (it.status === "failed") counts.failed++;
  }
  return { ...run, counts };
}
