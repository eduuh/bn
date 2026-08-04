import { lazy, Suspense, useState } from "react";
import { useSearchParams } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { AlertTriangle } from "lucide-react";
import {
  bragQuery,
  insightsQuery,
  metricsQuery,
  metricsTrendQuery,
  weeklyQuery,
} from "../api/client";
import type { MetricsTrendPoint, MetricsView } from "../api/types";
import { fmtAic, fmtRate } from "../lib/format";
import { StatGrid, StatTile } from "../components/StatTile";
import { Card } from "../components/ui/card";
import { Skeleton } from "../components/ui/skeleton";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "../components/ui/tabs";

const WINDOWS = [7, 30, 90];

/** Hours → a compact "3.2h" / "45m" label; `null` → an em-dash. */
function fmtHours(h: number | null): string {
  if (h == null) return "—";
  if (h < 1) return `${Math.round(h * 60)}m`;
  return `${h.toFixed(1)}h`;
}

/** Minutes → "2h 5m" / "45m". */
function fmtMinutes(min: number): string {
  const h = Math.floor(min / 60);
  const m = min % 60;
  return h > 0 ? `${h}h ${m}m` : `${m}m`;
}

/**
 * A tiny dependency-free SVG line chart of a single metric across recorded snapshots — the "small
 * chart" the tab shows above the tiles (#507). Pure SVG (no uPlot) keeps it trivially testable and
 * crisp on mobile. Renders nothing meaningful with <2 points; the caller shows the empty state.
 */
function TrendChart({ points }: { points: MetricsTrendPoint[] }) {
  const w = 320;
  const h = 72;
  const pad = 6;
  const ys = points.map((p) => p.merged);
  const max = Math.max(1, ...ys);
  const n = points.length;
  const x = (i: number) => (n <= 1 ? pad : pad + (i * (w - 2 * pad)) / (n - 1));
  const y = (v: number) => h - pad - (v / max) * (h - 2 * pad);
  const line = points.map((p, i) => `${x(i).toFixed(1)},${y(p.merged).toFixed(1)}`).join(" ");
  const area = `${pad},${h - pad} ${line} ${(w - pad).toFixed(1)},${h - pad}`;
  return (
    <svg
      viewBox={`0 0 ${w} ${h}`}
      className="h-[72px] w-full"
      role="img"
      aria-label="merged PRs per snapshot"
      preserveAspectRatio="none"
    >
      <polygon points={area} fill="hsl(var(--ok) / 0.15)" />
      <polyline
        points={line}
        fill="none"
        stroke="hsl(var(--ok))"
        strokeWidth={1.5}
        strokeLinejoin="round"
        strokeLinecap="round"
        vectorEffect="non-scaling-stroke"
      />
    </svg>
  );
}

/** The metric stat tiles + trend chart — the numeric spine of the tab. */
function MetricsPanel({ view, trend }: { view: MetricsView; trend: MetricsTrendPoint[] }) {
  const pr = view.pr;
  const agent = view.agent;
  return (
    <div className="space-y-4">
      <Card className="space-y-2 p-4">
        <div className="flex items-center justify-between">
          <h3 className="text-sm font-semibold">
            Merged PRs <span className="font-normal text-muted-foreground">per snapshot</span>
          </h3>
          <span className="text-xs text-muted-foreground">{trend.length} snapshots</span>
        </div>
        {trend.length >= 2 ? (
          <TrendChart points={trend} />
        ) : (
          <p className="py-4 text-xs text-muted-foreground">
            No trend yet — the weekly job (or <code>bn metrics --snapshot</code>) writes the
            snapshots this chart plots.
          </p>
        )}
      </Card>

      <StatGrid cols={6} className="border-y border-border">
        <StatTile label="PRs merged" value={String(pr.merged)} tone="green" />
        <StatTile label="in flight" value={String(pr.in_flight)} tone="blue" />
        <StatTile
          label="completion"
          value={pr.completion_rate == null ? "—" : fmtRate(pr.completion_rate)}
          tone="grey"
        />
        <StatTile label="cycle time (med)" value={fmtHours(pr.cycle_median_hours)} tone="grey" />
        <StatTile
          label="WIP notes"
          value={String(view.wip)}
          tone={view.wip > 5 ? "amber" : "grey"}
          title="Active notes with at least one open todo — the context-switch tax"
        />
        <StatTile label="open branches" value={String(view.open_branches)} tone="grey" />
      </StatGrid>

      <div className="space-y-2">
        <h3 className="text-[0.68rem] font-semibold uppercase tracking-[0.08em] text-muted-foreground">
          Agent telemetry
        </h3>
        <StatGrid cols={4} className="border-y border-border">
          <StatTile label="sessions" value={String(agent.sessions)} tone="grey" />
          <StatTile label="agent time" value={fmtMinutes(agent.total_minutes)} tone="grey" />
          <StatTile label="AIC" value={fmtAic(agent.cost_aiu)} tone="green" />
          <StatTile
            label="lines +/−"
            value={`${agent.lines_added}/${agent.lines_removed}`}
            tone="grey"
          />
        </StatGrid>
      </div>
    </div>
  );
}

/** A rendered text/markdown digest on the shared muted surface, with a loading/empty/error state. */
function DigestPanel({
  text,
  isLoading,
  isError,
  empty,
  label,
}: {
  text: string | undefined;
  isLoading: boolean;
  isError: boolean;
  empty: string;
  label: string;
}) {
  if (isLoading) return <Skeleton className="h-64 w-full" />;
  if (isError)
    return (
      <Card className="flex items-center gap-2 border-urgent/30 bg-urgent-muted p-4 text-sm text-urgent">
        <AlertTriangle className="h-4 w-4" />
        Could not load {label}.
      </Card>
    );
  if (!text || !text.trim())
    return <p className="py-6 text-sm text-muted-foreground">{empty}</p>;
  return (
    <pre className="overflow-auto whitespace-pre-wrap rounded-md border border-border bg-muted/60 p-4 font-mono text-xs leading-relaxed text-foreground/90">
      {text}
    </pre>
  );
}

const TABS = ["metrics", "cost", "weekly", "insights", "brag"];

/**
 * The cost panel is split out of this route's chunk on purpose: it pulls uPlot in with it, which
 * tripled the insights chunk (22 kB → 69 kB) when the two screens merged. Folding cost into this
 * page must not make the four non-cost tabs pay for a charting library they never draw with, so
 * the tab loads it on first open instead.
 */
const CostPanel = lazy(() =>
  import("../components/CostPanel").then((m) => ({ default: m.CostPanel })),
);

/**
 * `/insights` — Insights & Cost (#507), the reporting surface closing a dashboard-parity gap
 * (#488). One window picker drives the `bn metrics` scorecard (velocity/throughput tiles + a small
 * merged-PR trend chart and agent telemetry) *and* the `bn cost` dual-cost view, with three more
 * sub-tabs for the `bn weekly` / `bn insights` weekly digests and the `bn brag` accomplishments
 * packet. All read-only.
 *
 * Cost used to be its own `/cost` route with its own duplicate 7/30/90 picker, which meant answering
 * "how did the last 30 days go, and what did they cost" took two screens and two window selections
 * that could silently disagree. `/cost` now redirects here (see `main.tsx`).
 *
 * The active tab lives in `?tab=` so every panel is linkable — `/insights?tab=cost` is what the old
 * `/cost` link resolves to, and what the rail, the palette and RepoDetail's "view details" point at.
 * Switching tabs *replaces* the history entry rather than pushing one, so Back still returns to
 * wherever you came from instead of walking you through five tabs first. Radix unmounts inactive
 * panels, so the cost fetch — and the chunk it lives in — only load once its tab is opened; landing
 * on `/insights` costs exactly what it did before.
 */
export function Insights() {
  const [windowDays, setWindowDays] = useState(30);
  const [params, setParams] = useSearchParams();
  const tab = TABS.includes(params.get("tab") ?? "") ? params.get("tab")! : "metrics";
  const metrics = useQuery(metricsQuery(windowDays));
  const trend = useQuery(metricsTrendQuery());
  const weekly = useQuery(weeklyQuery());
  const insights = useQuery(insightsQuery());
  const brag = useQuery(bragQuery());

  return (
    // One `Tabs` root spanning the whole screen: the view switcher lives in the header bar and its
    // panels own the scrolling body, so flipping tabs never moves the controls. Previously the head,
    // both switchers and the panel all stacked in a document, which pushed the chart below the fold
    // and left the window picker floating over an empty measure.
    <Tabs
      value={tab}
      onValueChange={(v) => {
        // `replace` so tab-flipping doesn't bury the entry the user actually navigated from.
        const next = new URLSearchParams(params);
        if (v === "metrics") next.delete("tab");
        else next.set("tab", v);
        setParams(next, { replace: true });
      }}
      className="flex h-full min-h-0 flex-col"
    >
      <header className="flex flex-none flex-wrap items-center gap-x-3 gap-y-2 border-b border-border px-4 py-2.5">
        <h1 className="font-display text-lg font-semibold tracking-tight text-foreground">
          Insights &amp; Cost
        </h1>
        <span className="hidden font-mono text-[11px] text-muted-foreground/70 lg:inline">
          velocity · throughput · spend · weekly digests
        </span>

        <TabsList aria-label="insights view" className="ml-1">
          <TabsTrigger value="metrics">Metrics</TabsTrigger>
          <TabsTrigger value="cost">Cost</TabsTrigger>
          <TabsTrigger value="weekly">Weekly</TabsTrigger>
          <TabsTrigger value="insights">Insights</TabsTrigger>
          <TabsTrigger value="brag">Brag</TabsTrigger>
        </TabsList>

        {/* One picker for the whole page — it windows the cost view as well as the metrics, which
            is the point of folding the two screens together. */}
        <Tabs
          value={String(windowDays)}
          onValueChange={(v) => setWindowDays(Number(v))}
          className="ml-auto"
        >
          <TabsList aria-label="window">
            {WINDOWS.map((d) => (
              <TabsTrigger key={d} value={String(d)}>
                {d}d
              </TabsTrigger>
            ))}
          </TabsList>
        </Tabs>
      </header>

      <div className="min-h-0 flex-1 overflow-y-auto">
        <TabsContent value="metrics" className="mt-0 px-4 py-4">
          {metrics.isLoading && <Skeleton className="h-72 w-full" />}
          {metrics.isError && (
            <div className="flex items-center gap-2 rounded-lg border border-urgent/30 bg-urgent-muted p-4 text-sm text-urgent">
              <AlertTriangle className="h-4 w-4" />
              Could not load metrics.
            </div>
          )}
          {metrics.data && (
            <MetricsPanel view={metrics.data} trend={trend.data?.snapshots ?? []} />
          )}
        </TabsContent>

        <TabsContent value="cost" className="mt-0 px-4 py-4">
          <Suspense fallback={<Skeleton className="h-72 w-full" />}>
            <CostPanel windowDays={windowDays} />
          </Suspense>
        </TabsContent>

        {/* The digests are prose. They keep a reading measure even though the page is full-bleed —
            a 1900px line of markdown is unreadable, and "dense" is not the same as "edge to edge". */}
        <TabsContent value="weekly" className="mt-0 max-w-[60rem] px-4 py-4">
          <DigestPanel
            text={weekly.data?.rendered}
            isLoading={weekly.isLoading}
            isError={weekly.isError}
            empty="No weekly review yet — closed goals show up here as they accrue."
            label="the weekly review"
          />
        </TabsContent>

        <TabsContent value="insights" className="mt-0 max-w-[60rem] px-4 py-4">
          <DigestPanel
            text={insights.data?.rendered}
            isLoading={insights.isLoading}
            isError={insights.isError}
            empty="No insights yet — the auto-retro mines your telemetry once there's enough history."
            label="insights"
          />
        </TabsContent>

        <TabsContent value="brag" className="mt-0 max-w-[60rem] px-4 py-4">
          <DigestPanel
            text={brag.data?.rendered}
            isLoading={brag.isLoading}
            isError={brag.isError}
            empty="Nothing shipped in the window yet — merged work shows up here."
            label="the brag packet"
          />
        </TabsContent>
      </div>
    </Tabs>
  );
}
