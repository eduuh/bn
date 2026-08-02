import { Link } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { AlertTriangle, Loader2, Plus, ServerCrash } from "lucide-react";
import { fleetQuery, orchestrateQuery } from "../api/client";
import type { Counts } from "../api/types";
import { fleetEntries, needsYou, orchestrators } from "../lib/fleet";
import { useNow } from "../hooks/useNow";
import { useToasts } from "../hooks/useToasts";
import { useBacklogActions } from "../hooks/useBacklogActions";
import { FleetEntryCard } from "../components/FleetEntryCard";
import { NodeStrip } from "../components/NodeStrip";
import { PageHead } from "../components/PageHead";
import { LanesPanel } from "../components/backlog/LanesPanel";
import { QueuePanel } from "../components/backlog/QueuePanel";
import { RecoveryFeed } from "../components/RecoveryFeed";
import { Toasts } from "../components/backlog/Toasts";
import { Badge } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import { Card } from "../components/ui/card";
import { Skeleton } from "../components/ui/skeleton";
import { cn } from "../lib/utils";

/** The per-run count strip (queued · in-flight · merged · failed). */
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

/** A section header — title + a live count badge. `tone` colours the count for the needs tier. */
function SectionHead({
  title,
  count,
  tone,
}: {
  title: string;
  count: number;
  tone?: "warn" | "info";
}) {
  return (
    <div className="flex items-center gap-2">
      <h3 className="text-sm font-semibold uppercase tracking-wide text-muted-foreground">
        {title}
      </h3>
      <Badge variant={tone ?? "muted"}>{count}</Badge>
    </div>
  );
}

/** Responsive card grid shared by every fleet tier. */
function CardGrid({ children }: { children: React.ReactNode }) {
  return <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">{children}</div>;
}

/**
 * `/fleet` — the Fleet (design §479, job 1: *Glance*). Who needs you (stuck / plan-waiting agents
 * pinned at the top), then everything working, as glanceable cards patched live by the SSE stream.
 * Per-run count strips fold out to the backlog controls (concurrency scale, stop, queue
 * reorder/remove) so the whole fleet is steerable one-handed.
 */
export function Fleet() {
  const orch = useQuery(orchestrateQuery());
  const fleet = useQuery(fleetQuery());
  const now = useNow();
  const { toasts, push, dismiss } = useToasts();
  const actions = useBacklogActions(push);

  const entries = fleetEntries(orch.data, fleet.data);
  const orchestratorEntries = orchestrators(entries);
  // Orchestrators own the pinned top tier, so keep them out of the needs-you/working tiers below.
  const rest = entries.filter((e) => e.role !== "orchestrator");
  const need = needsYou(rest);
  const working = rest.filter((e) => !e.needs);
  const runs = orch.data?.runs ?? [];
  // Just-dispatched agents with no transcript yet (#545) — the pending "Starting…" tier.
  const starting = fleet.data?.starting ?? 0;
  // OR, not AND: either query still pending means we don't yet know the fleet — a single
  // fast-resolving empty query must not drop us into the "0 running / no agents" empty state (#544).
  const loading = orch.isLoading || fleet.isLoading;
  const nothing = !loading && entries.length === 0 && runs.length === 0 && starting === 0;
  const manageBusy =
    actions.pending.scale || actions.pending.stop || actions.pending.reorder || actions.pending.remove;

  return (
    <div className="space-y-6 pb-8">
      <PageHead
        title="Fleet"
        sub={loading ? "loading…" : `${entries.length} running · live`}
        right={
          <Button asChild size="sm">
            <Link to="/start">
              <Plus className="h-4 w-4" /> Start
            </Link>
          </Button>
        }
      />

      {/* The machines, not their gauges — CPU and RAM now ride in the topbar on every screen, so
          repeating the full machine card here would spend the top of the fleet on numbers you can
          already see. */}
      <NodeStrip />

      {loading && (
        <div className="space-y-3">
          <div className="flex items-center gap-2 text-sm text-muted-foreground">
            <Loader2 className="h-4 w-4 animate-spin" />
            Loading fleet…
          </div>
          <CardGrid>
            {[0, 1, 2, 3].map((i) => (
              <Card key={i} className="space-y-2 p-3 pl-4">
                <Skeleton className="h-4 w-1/2" />
                <Skeleton className="h-3 w-2/3" />
                <Skeleton className="h-3 w-1/3" />
              </Card>
            ))}
          </CardGrid>
        </div>
      )}

      {orch.isError && (
        <Card className="flex items-center gap-2 border-urgent/30 bg-urgent-muted p-4 text-sm text-urgent">
          <AlertTriangle className="h-4 w-4" />
          Could not load orchestration status.
        </Card>
      )}

      {nothing && (
        <Card className="flex flex-col items-center gap-3 px-6 py-12 text-center">
          <ServerCrash className="h-8 w-8 text-muted-foreground" />
          <div className="space-y-1">
            <h3 className="text-lg font-semibold">No agents running</h3>
            <p className="text-sm text-muted-foreground">Nothing is in flight right now.</p>
          </div>
          <Button asChild>
            <Link to="/start">
              <Plus className="h-4 w-4" /> Start an agent
            </Link>
          </Button>
        </Card>
      )}

      {orchestratorEntries.length > 0 && (
        <section className="space-y-3" aria-label="Orchestrators">
          <SectionHead title="Orchestrators" count={orchestratorEntries.length} tone="info" />
          <CardGrid>
            {orchestratorEntries.map((e) => (
              <FleetEntryCard key={e.key} entry={e} now={now} harnesses={fleet.data?.harnesses} />
            ))}
          </CardGrid>
        </section>
      )}

      {need.length > 0 && (
        <section className="space-y-3" aria-label="Needs you">
          <SectionHead title="Needs you" count={need.length} tone="warn" />
          <CardGrid>
            {need.map((e) => (
              <FleetEntryCard key={e.key} entry={e} now={now} harnesses={fleet.data?.harnesses} />
            ))}
          </CardGrid>
        </section>
      )}

      {working.length > 0 && (
        <section className="space-y-3" aria-label="Working">
          <SectionHead title="Working" count={working.length} />
          <CardGrid>
            {working.map((e) => (
              <FleetEntryCard key={e.key} entry={e} now={now} harnesses={fleet.data?.harnesses} />
            ))}
          </CardGrid>
        </section>
      )}

      {starting > 0 && (
        <section className="space-y-3" aria-label="Starting">
          <SectionHead title="Starting…" count={starting} tone="info" />
          <CardGrid>
            {Array.from({ length: starting }).map((_, i) => (
              <Card
                key={i}
                className="relative flex items-center gap-3 overflow-hidden p-3 pl-4"
              >
                <span className="absolute inset-y-0 left-0 w-1 bg-info" aria-hidden="true" />
                <Loader2 className="h-4 w-4 shrink-0 animate-spin text-info" />
                <div className="space-y-2">
                  <div className="text-sm font-medium text-foreground">Starting…</div>
                  <Skeleton className="h-3 w-40" />
                </div>
              </Card>
            ))}
          </CardGrid>
        </section>
      )}

      {runs.map((run) => (
        <section className="space-y-2" key={run.repo}>
          <div className="flex flex-wrap items-center justify-between gap-2 rounded-lg border border-border bg-card px-3 py-2">
            <span className="font-mono text-sm font-medium text-foreground">{run.repo}</span>
            <MiniCounts counts={run.counts} />
          </div>
          <details className="manage">
            <summary>Manage run</summary>
            <LanesPanel
              run={run}
              busy={manageBusy}
              onScale={(c) => actions.setConcurrency(run.repo, c)}
              onStop={() => actions.stopRun(run.repo)}
            />
            <QueuePanel
              run={run}
              busy={manageBusy}
              onReorder={(ids) => actions.reorderQueue(run.repo, ids)}
              onRemove={(id) => actions.removeItem(run.repo, id)}
            />
          </details>
        </section>
      ))}

      <RecoveryFeed />
      <Toasts toasts={toasts} onDismiss={dismiss} />
    </div>
  );
}
