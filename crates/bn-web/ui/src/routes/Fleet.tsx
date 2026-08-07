import { useState } from "react";
import { Link } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { AlertTriangle, Loader2, Plus, ServerCrash } from "lucide-react";
import { fleetQuery, orchestrateQuery } from "../api/client";
import type { Counts } from "../api/types";
import { fleetEntries, needsYou, orchestrators } from "../lib/fleet";
import { useNow } from "../hooks/useNow";
import { useBacklogActions } from "../hooks/useBacklogActions";
import { FleetEntryCard } from "../components/FleetEntryCard";
import { FleetRow } from "../components/FleetRow";
import { NodeStrip } from "../components/NodeStrip";
import { PendingApprovals } from "../components/PendingApprovals";
import { PaneRule, Section } from "../components/ui/pane";
import { AgentActivity } from "../components/AgentActivity";
import { SteerPanel } from "../components/SteerPanel";
import { ApprovalPanel } from "../components/ApprovalPanel";
import { LanesPanel } from "../components/backlog/LanesPanel";
import { QueuePanel } from "../components/backlog/QueuePanel";
import { RecoveryFeed } from "../components/RecoveryFeed";
import { ToastStack } from "../components/ToastStack";
import { Button } from "../components/ui/button";
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
  const actions = useBacklogActions();

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
    actions.pending.scale ||
    actions.pending.stop ||
    actions.pending.reorder ||
    actions.pending.remove;

  const [selectedKey, setSelectedKey] = useState<string | null>(null);

  // Which agent the detail pane is watching. Defaults to the one that most needs a human — the
  // board's whole point — and falls back to the first live worker so the pane is never empty while
  // agents are running.
  // The type predicate is what makes `selected.sessionId` a `string` downstream. A plain
  // `.filter((e) => e.sessionId)` narrows nothing, which is why the detail pane used to assert
  // non-null on every use of it.
  const watchable = [...need, ...orchestratorEntries, ...working].filter(
    (e): e is typeof e & { sessionId: string } => Boolean(e.sessionId),
  );
  const selected = watchable.find((e) => e.key === selectedKey) ?? watchable[0];

  return (
    <div className="flex h-full min-h-0 flex-col">
      <header className="flex flex-none items-baseline gap-3 border-b border-border px-4 py-2.5">
        <h1 className="font-display text-lg font-semibold tracking-tight text-foreground">Fleet</h1>
        <span className="font-mono text-[11px] tabular-nums text-muted-foreground/70">
          {loading ? "loading…" : `${entries.length} running · live`}
        </span>
        <div className="ml-auto">
          <Button asChild size="sm">
            <Link to="/start">
              <Plus className="h-4 w-4" /> Start
            </Link>
          </Button>
        </div>
      </header>

      {/* The board on the left, the agent you are watching on the right. Reading a transcript used
          to cost a navigation to `/agents/:id` and the loss of every other agent's state; here the
          list stays put and the stream rides beside it. */}
      <div className="grid min-h-0 flex-1 grid-cols-1 lg:grid-cols-[26rem_minmax(0,1fr)]">
        <aside
          aria-label="fleet board"
          className="flex min-h-0 flex-col overflow-y-auto border-border lg:border-r"
        >
          <NodeStrip />
          {/* Above every tier: an agent blocked on a dialog is the only thing on this board
              that is waiting on *you*, and it should be answerable without opening it. */}
          <PendingApprovals />

          {loading && (
            <div className="space-y-2 px-3.5 py-3">
              <div className="flex items-center gap-2 text-sm text-muted-foreground">
                <Loader2 className="h-4 w-4 animate-spin" />
                Loading fleet…
              </div>
              {[0, 1, 2, 3].map((i) => (
                <Skeleton key={i} className="h-12 w-full" />
              ))}
            </div>
          )}

          {orch.isError && (
            <div className="m-3.5 flex items-center gap-2 rounded-lg border border-urgent/30 bg-urgent-muted p-3 text-sm text-urgent">
              <AlertTriangle className="h-4 w-4" />
              Could not load orchestration status.
            </div>
          )}

          {nothing && (
            <div className="flex flex-col items-center gap-3 px-6 py-12 text-center">
              <ServerCrash className="h-8 w-8 text-muted-foreground" />
              <div className="space-y-1">
                <h3 className="font-display text-base font-semibold">No agents running</h3>
                <p className="text-sm text-muted-foreground">Nothing is in flight right now.</p>
              </div>
              <Button asChild size="sm">
                <Link to="/start">
                  <Plus className="h-4 w-4" /> Start an agent
                </Link>
              </Button>
            </div>
          )}

          {orchestratorEntries.length > 0 && (
            <Section label="Orchestrators" meta={String(orchestratorEntries.length)}>
              <div className="space-y-2">
                {orchestratorEntries.map((e) => (
                  <FleetEntryCard
                    key={e.key}
                    entry={e}
                    now={now}
                    harnesses={fleet.data?.harnesses}
                  />
                ))}
              </div>
            </Section>
          )}

          {need.length > 0 && (
            <>
              {orchestratorEntries.length > 0 && <PaneRule />}
              <Section label="Needs you" meta={String(need.length)}>
                <div className="space-y-2">
                  {need.map((e) => (
                    <FleetEntryCard
                      key={e.key}
                      entry={e}
                      now={now}
                      harnesses={fleet.data?.harnesses}
                    />
                  ))}
                </div>
              </Section>
            </>
          )}

          {/* Rows, not cards: nothing in this tier is waiting on a decision, so it earns density —
              and it keeps a card on this board meaning "this one needs you". */}
          {working.length > 0 && (
            <>
              <PaneRule />
              <Section label="Working" meta={String(working.length)}>
                <div className="space-y-0.5">
                  {working.map((e) => (
                    <FleetRow
                      key={e.key}
                      entry={e}
                      now={now}
                      harnesses={fleet.data?.harnesses}
                      compact
                      selected={selected?.key === e.key}
                      onSelect={e.sessionId ? () => setSelectedKey(e.key) : undefined}
                    />
                  ))}
                </div>
              </Section>
            </>
          )}

          {starting > 0 && (
            <>
              <PaneRule />
              <Section label="Starting…" meta={String(starting)}>
                {Array.from({ length: starting }).map((_, i) => (
                  <div key={i} className="flex items-center gap-2 py-1.5">
                    <Loader2 className="h-3.5 w-3.5 shrink-0 animate-spin text-info" />
                    <span className="text-sm text-muted-foreground">Starting…</span>
                    <Skeleton className="h-3 w-40" />
                  </div>
                ))}
              </Section>
            </>
          )}

          {runs.map((run) => (
            <div key={run.repo}>
              <PaneRule />
              <Section label={run.repo} meta="run">
                <MiniCounts counts={run.counts} />
                <details className="manage mt-2">
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
              </Section>
            </div>
          ))}

          <PaneRule />
          <RecoveryFeed />
        </aside>

        <section aria-label="agent transcript" className="flex min-h-0 flex-col overflow-y-auto">
          {selected ? (
            <>
              <div className="flex flex-none flex-wrap items-baseline gap-2 border-b border-border px-3.5 py-2.5">
                <h2 className="font-mono text-sm font-semibold text-foreground">
                  {selected.branch}
                </h2>
                <span className="font-mono text-[11px] text-muted-foreground/70">
                  {selected.repo} · {selected.stateLabel}
                </span>
                <div className="ml-auto">
                  <Button asChild variant="ghost" size="sm">
                    <Link to={`/agents/${encodeURIComponent(selected.sessionId)}`}>
                      Open detail
                    </Link>
                  </Button>
                </div>
              </div>
              <div className="space-y-3 px-3.5 py-3">
                {/* If this agent is blocked, the thing you came to do is unblock it — so its
                    dialog leads, above the transcript. Renders nothing otherwise. */}
                <ApprovalPanel target={selected.target} />
                <AgentActivity sessionId={selected.sessionId} />
                <SteerPanel target={selected.target} />
              </div>
            </>
          ) : (
            <div className="flex flex-1 flex-col items-center justify-center gap-2 px-6 text-center">
              <p className="text-sm text-muted-foreground">
                {loading ? "Loading the fleet…" : "No live agent to watch."}
              </p>
              <p className="max-w-sm text-xs text-muted-foreground/70">
                Pick a working agent on the left to read its transcript here.
              </p>
            </div>
          )}
        </section>
      </div>

      <ToastStack />
    </div>
  );
}
