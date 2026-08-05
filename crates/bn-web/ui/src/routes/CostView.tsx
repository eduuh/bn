import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { AlertTriangle } from "lucide-react";
import { costQuery } from "../api/client";
import { fmtAic, fmtTokens, fmtUsd } from "../lib/format";
import { categoryColor, categoryLabel, presentCategories, type CostMetric } from "../lib/cost";
import { StatTile } from "../components/StatTile";
import { CostChart } from "../components/CostChart";
import { CostTable, type CostDimension } from "../components/CostTable";
import { CostLive } from "../components/CostLive";
import { Card } from "../components/ui/card";
import { Skeleton } from "../components/ui/skeleton";
import { Tabs, TabsList, TabsTrigger } from "../components/ui/tabs";

const WINDOWS = [7, 30, 90];
const METRICS: { key: CostMetric; label: string }[] = [
  { key: "output_tokens", label: "Output tokens" },
  { key: "aic", label: "AIC" },
  { key: "cost_usd", label: "$ (API-eq)" },
];

/** Skeleton while the cost view loads — tiles + chart placeholder so the page never flashes an
 * empty body or a misleading zeroed state (#544). */
function LoadingSkeleton() {
  return (
    <div className="space-y-5" aria-hidden="true">
      <div className="flex gap-3">
        {[0, 1, 2, 3, 4].map((i) => (
          <Card key={i} className="flex flex-1 flex-col gap-2 p-4">
            <Skeleton className="h-7 w-2/3" />
            <Skeleton className="h-3 w-1/2" />
          </Card>
        ))}
      </div>
      <Card className="space-y-3 p-4">
        <Skeleton className="h-4 w-40" />
        <Skeleton className="h-56 w-full" />
      </Card>
    </div>
  );
}

/**
 * `/cost` — Cost & Usage (design §4(b)), the #381 dual-cost surface. Stat tiles (AIC + $, tokens,
 * sessions, machines), a uPlot stacked-bar chart of usage by day coloured by work-class category,
 * a breakdown table with a `by` switcher mirroring `bn cost`'s columns, and a live in-flight
 * section fed by the SSE `fleet` frame. The window picker drives the refetch; the table dimension
 * and chart metric are client-side selections over the single fetched view.
 */
export function CostView() {
  const [windowDays, setWindowDays] = useState(30);
  const [by, setBy] = useState<CostDimension>("repo");
  const [metric, setMetric] = useState<CostMetric>("output_tokens");
  const cost = useQuery(costQuery(windowDays));

  const view = cost.data;
  const cats = view ? presentCategories(view.by_day_category) : [];
  // With a single work-class present (e.g. everything folds to Unattributed), a "stacked by
  // category" chart is a degenerate single flat series — hide it and point at the real breakdowns.
  const categoryChartDegenerate = cats.length <= 1;

  return (
    <div className="mx-auto max-w-3xl space-y-5 pb-8">
      <header className="flex flex-wrap items-center justify-between gap-3">
        <div className="space-y-1">
          <h2 className="text-2xl font-semibold tracking-tight">Cost &amp; Usage</h2>
          <p className="text-sm text-muted-foreground">
            <span className="font-medium text-ok">AIC</span> = metered Copilot bill ·{" "}
            <span className="font-medium text-info">$</span> = cache-aware API-equiv benchmark (not a
            bill)
          </p>
        </div>
        <Tabs value={String(windowDays)} onValueChange={(v) => setWindowDays(Number(v))}>
          <TabsList aria-label="window">
            {WINDOWS.map((w) => (
              <TabsTrigger key={w} value={String(w)}>
                {w}d
              </TabsTrigger>
            ))}
          </TabsList>
        </Tabs>
      </header>

      {cost.isLoading && <LoadingSkeleton />}

      {cost.isError && (
        <Card className="flex items-center gap-2 border-urgent/30 bg-urgent-muted p-4 text-sm text-urgent">
          <AlertTriangle className="h-4 w-4" />
          Could not load cost &amp; usage.
        </Card>
      )}

      {view && (
        <>
          <div className="flex flex-wrap gap-3">
            <StatTile
              label="AIC · Copilot bill"
              value={fmtAic(view.totals.aic)}
              tone="green"
              title="AI Credits — the real, metered Copilot bill"
            />
            <StatTile
              label="$ · API-equiv"
              value={fmtUsd(view.totals.cost_usd)}
              tone="blue"
              title="Cache-aware benchmark at public per-model API prices — a benchmark, not a bill"
            />
            <StatTile label="output tokens" value={fmtTokens(view.totals.output_tokens)} tone="grey" />
            <StatTile label="sessions" value={String(view.totals.sessions)} tone="grey" />
            <StatTile label="active machines" value={String(view.by_machine.length)} tone="grey" />
          </div>

          <Card className="space-y-3 p-4">
            <div className="flex flex-wrap items-center justify-between gap-3">
              <h3 className="text-sm font-semibold">
                Usage by day{" "}
                <span className="font-normal text-muted-foreground">
                  {categoryChartDegenerate ? "by day" : "stacked by category"}
                </span>
              </h3>
              <Tabs value={metric} onValueChange={(v) => setMetric(v as CostMetric)}>
                <TabsList aria-label="chart metric">
                  {METRICS.map((m) => (
                    <TabsTrigger key={m.key} value={m.key}>
                      {m.label}
                    </TabsTrigger>
                  ))}
                </TabsList>
              </Tabs>
            </div>

            <CostChart cells={view.by_day_category} metric={metric} />

            {categoryChartDegenerate ? (
              <p className="text-xs text-muted-foreground">
                Only one work-class present
                {cats.length === 1 ? ` (${categoryLabel(cats[0])})` : ""} — the by-category split is
                uniform. Use the breakdown below (by repo / model / machine) for real signal.
              </p>
            ) : (
              <div className="flex flex-wrap gap-x-4 gap-y-1.5 text-xs text-muted-foreground">
                {cats.map((c, i) => (
                  <span key={c} className="inline-flex items-center gap-1.5">
                    <span
                      className="h-2.5 w-2.5 rounded-sm"
                      style={{ background: categoryColor(c, i) }}
                    />
                    {categoryLabel(c)}
                  </span>
                ))}
              </div>
            )}
          </Card>

          <Card className="p-4">
            <CostTable view={view} by={by} onBy={setBy} />
          </Card>

          <CostLive />
        </>
      )}
    </div>
  );
}
