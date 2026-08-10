import { useQuery } from "@tanstack/react-query";
import { ChevronRight } from "lucide-react";

import { orchestrateQuery } from "../api/client";
import type { Counts } from "../api/types";
import { useBacklogActions } from "../hooks/useBacklogActions";
import { LanesPanel } from "./backlog/LanesPanel";
import { QueuePanel } from "./backlog/QueuePanel";
import { cn } from "../lib/utils";

/**
 * Per-run steering: the count strip, the concurrency scale / stop control, and the queue
 * reorder / remove list, for every live orchestrate run.
 *
 * Extracted from `/fleet` because it was the only thing on that screen `/queue` could not already
 * do, and `/queue` is where the rest of the operational verbs live. Kept as one component used by
 * both rather than copied: these controls own a write path (scale, stop, reorder, remove) whose
 * optimistic patching and rollback live in `useBacklogActions`, and two divergent copies of a write
 * surface is how a dashboard ends up with a button that works on one page and not the other.
 *
 * Self-contained on purpose — it reads its own runs and owns its own action hook — so a page adds
 * it with `<RunControls />` and nothing else. The caller supplies only the wrapper.
 */
function MiniCounts({ counts }: { counts: Counts }) {
  const stat = (n: number, label: string, tone?: string) => (
    <span className="inline-flex items-baseline gap-1">
      <b className={cn("tabular-nums text-foreground", tone)}>{n}</b>
      <span className="text-muted-foreground">{label}</span>
    </span>
  );
  return (
    <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-xs">
      {stat(counts.queued, "queued")}
      {stat(counts.in_flight, "in-flight")}
      {stat(counts.merged, "merged", counts.merged > 0 ? "text-ok" : undefined)}
      {counts.failed > 0 && stat(counts.failed, "failed", "text-urgent")}
    </div>
  );
}

/** One run's counts plus its fold-out controls. */
export function RunControl({ repo }: { repo: string }) {
  const orch = useQuery(orchestrateQuery());
  const actions = useBacklogActions();
  const run = (orch.data?.runs ?? []).find((r) => r.repo === repo);
  const busy =
    actions.pending.scale ||
    actions.pending.stop ||
    actions.pending.reorder ||
    actions.pending.remove;

  if (!run) return null;
  return (
    <div className="space-y-2">
      <MiniCounts counts={run.counts} />
      <details className="group mb-3">
        <summary className="cursor-pointer list-none py-2 text-sm font-semibold text-muted-foreground [&::-webkit-details-marker]:hidden">
          <ChevronRight className="mr-1 inline h-3.5 w-3.5 transition-transform group-open:rotate-90" />
          Manage run
        </summary>
        <LanesPanel
          run={run}
          busy={busy}
          onScale={(c) => actions.setConcurrency(run.repo, c)}
          onStop={() => actions.stopRun(run.repo)}
        />
        <QueuePanel
          run={run}
          busy={busy}
          onReorder={(ids) => actions.reorderQueue(run.repo, ids)}
          onRemove={(id) => actions.removeItem(run.repo, id)}
        />
      </details>
    </div>
  );
}

/** Every live run, each with its own controls. Renders nothing when no run is live. */
export function RunControls({ className }: { className?: string }) {
  const orch = useQuery(orchestrateQuery());
  const runs = orch.data?.runs ?? [];
  if (runs.length === 0) return null;
  return (
    <div className={cn("space-y-4", className)}>
      {runs.map((run) => (
        <div key={run.repo} className="space-y-1.5">
          <div className="font-mono text-[11px] uppercase tracking-wide text-muted-foreground">
            {run.repo}
          </div>
          <RunControl repo={run.repo} />
        </div>
      ))}
    </div>
  );
}
