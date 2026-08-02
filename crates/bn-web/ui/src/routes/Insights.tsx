import { useState } from "react";
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
import { StatTile } from "../components/StatTile";
import { Card } from "../components/ui/card";
import { Skeleton } from "../components/ui/skeleton";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "../components/ui/tabs";
import { PageHead } from "../components/PageHead";

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

      <div className="flex flex-wrap gap-3">
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
      </div>

      <div className="space-y-2">
        <h3 className="text-sm font-semibold text-muted-foreground">Agent telemetry</h3>
        <div className="flex flex-wrap gap-3">
          <StatTile label="sessions" value={String(agent.sessions)} tone="grey" />
          <StatTile label="agent time" value={fmtMinutes(agent.total_minutes)} tone="grey" />
          <StatTile label="AIC" value={fmtAic(agent.cost_aiu)} tone="green" />
          <StatTile
            label="lines +/−"
            value={`${agent.lines_added}/${agent.lines_removed}`}
            tone="grey"
          />
        </div>
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

/**
 * `/insights` — Insights & Metrics (#507), the reporting surface closing a dashboard-parity gap
 * (#488). A window picker drives the `bn metrics` scorecard (velocity/throughput tiles + a small
 * merged-PR trend chart and agent telemetry), and three sub-tabs surface the `bn weekly` /
 * `bn insights` weekly digests and the `bn brag` accomplishments packet. All read-only.
 */
export function Insights() {
  const [windowDays, setWindowDays] = useState(30);
  const metrics = useQuery(metricsQuery(windowDays));
  const trend = useQuery(metricsTrendQuery());
  const weekly = useQuery(weeklyQuery());
  const insights = useQuery(insightsQuery());
  const brag = useQuery(bragQuery());

  return (
    <div className="space-y-5 pb-8">
      <PageHead
        title="Insights & Metrics"
        sub="velocity · throughput · weekly digests"
        right={
          <Tabs value={String(windowDays)} onValueChange={(v) => setWindowDays(Number(v))}>
            <TabsList aria-label="metrics window">
              {WINDOWS.map((d) => (
                <TabsTrigger key={d} value={String(d)}>
                  {d}d
                </TabsTrigger>
              ))}
            </TabsList>
          </Tabs>
        }
      />

      <Tabs defaultValue="metrics">
        <TabsList aria-label="insights view">
          <TabsTrigger value="metrics">Metrics</TabsTrigger>
          <TabsTrigger value="weekly">Weekly</TabsTrigger>
          <TabsTrigger value="insights">Insights</TabsTrigger>
          <TabsTrigger value="brag">Brag</TabsTrigger>
        </TabsList>

        <TabsContent value="metrics" className="mt-4">
          {metrics.isLoading && <Skeleton className="h-72 w-full" />}
          {metrics.isError && (
            <Card className="flex items-center gap-2 border-urgent/30 bg-urgent-muted p-4 text-sm text-urgent">
              <AlertTriangle className="h-4 w-4" />
              Could not load metrics.
            </Card>
          )}
          {metrics.data && (
            <MetricsPanel view={metrics.data} trend={trend.data?.snapshots ?? []} />
          )}
        </TabsContent>

        <TabsContent value="weekly" className="mt-4">
          <DigestPanel
            text={weekly.data?.rendered}
            isLoading={weekly.isLoading}
            isError={weekly.isError}
            empty="No weekly review yet — closed goals show up here as they accrue."
            label="the weekly review"
          />
        </TabsContent>

        <TabsContent value="insights" className="mt-4">
          <DigestPanel
            text={insights.data?.rendered}
            isLoading={insights.isLoading}
            isError={insights.isError}
            empty="No insights yet — the auto-retro mines your telemetry once there's enough history."
            label="insights"
          />
        </TabsContent>

        <TabsContent value="brag" className="mt-4">
          <DigestPanel
            text={brag.data?.rendered}
            isLoading={brag.isLoading}
            isError={brag.isError}
            empty="Nothing shipped in the window yet — merged work shows up here."
            label="the brag packet"
          />
        </TabsContent>
      </Tabs>
    </div>
  );
}
