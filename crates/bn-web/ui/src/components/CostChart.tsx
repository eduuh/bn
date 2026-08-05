import { useEffect, useMemo, useRef } from "react";
import uPlot from "uplot";
import type { DayCategory } from "../api/types";
import { fmtTokens } from "../lib/format";
import { buildStackModel, categoryColor, stackForDraw, type CostMetric } from "../lib/cost";

const AXIS = "#8b949e";
const GRID = "#21262d";

/** Format a y-axis tick for the chart's current metric: token counts compactly, cost units as
 * plain / dollar numbers (rounded — axis ticks don't need cents). */
function fmtAxis(metric: CostMetric, v: number): string {
  if (metric === "output_tokens") return fmtTokens(v);
  if (metric === "cost_usd") return `$${Math.round(v)}`;
  return String(Math.round(v));
}

/**
 * uPlot stacked bars: one bar per day, stacked by work-class category (#391 §4(b)). The per-day
 * per-category values come from the `by_day_category` cross-tab; [`stackForDraw`] pre-cumulates
 * them and orders the series tallest-first so uPlot's later-on-top paint order leaves each
 * category's slice visible (opaque fills). Cost history moves slowly, so the plot is simply
 * created/destroyed when the data (or width) changes rather than live-patched per frame.
 */
export function CostChart({
  cells,
  metric,
  height = 260,
}: {
  cells: DayCategory[];
  metric: CostMetric;
  height?: number;
}) {
  const elRef = useRef<HTMLDivElement>(null);
  const model = useMemo(() => buildStackModel(cells, metric), [cells, metric]);

  useEffect(() => {
    const el = elRef.current;
    if (!el || model.days.length === 0) return;

    const { days, categories, values } = model;
    const { order, cumulative } = stackForDraw(values);
    const xs = days.map((_, i) => i);
    const data: uPlot.AlignedData = [xs, ...cumulative];

    const bars = uPlot.paths.bars!({ size: [0.68, 60], align: 0 });
    const series: uPlot.Series[] = [
      {},
      ...order.map((catIdx) => {
        const color = categoryColor(categories[catIdx], catIdx);
        return {
          label: categories[catIdx],
          stroke: color,
          fill: color,
          width: 0,
          paths: bars,
          points: { show: false },
        } as uPlot.Series;
      }),
    ];

    const opts: uPlot.Options = {
      width: el.clientWidth || 640,
      height,
      legend: { show: false },
      cursor: { show: false },
      scales: {
        x: { time: false, range: () => [-0.5, days.length - 0.5] },
        y: { range: (_u, _min, max) => [0, max && max > 0 ? max : 1] },
      },
      axes: [
        {
          stroke: AXIS,
          grid: { show: false },
          ticks: { show: false },
          font: "11px ui-monospace, monospace",
          // Show at most ~12 day ticks so a 90-day window stays legible.
          splits: () => {
            const step = Math.max(1, Math.ceil(days.length / 12));
            const out: number[] = [];
            for (let i = 0; i < days.length; i += step) out.push(i);
            return out;
          },
          values: (_u, splits) =>
            splits.map((v) => {
              const d = days[Math.round(v)];
              return d ? d.slice(5) : "";
            }),
        },
        {
          stroke: AXIS,
          grid: { stroke: GRID, width: 1 },
          ticks: { show: false },
          font: "11px ui-monospace, monospace",
          size: 54,
          values: (_u, splits) => splits.map((v) => fmtAxis(metric, v)),
        },
      ],
      series,
    };

    const u = new uPlot(opts, data, el);
    const ro = new ResizeObserver(() => u.setSize({ width: el.clientWidth || 640, height }));
    ro.observe(el);
    return () => {
      ro.disconnect();
      u.destroy();
    };
  }, [model, metric, height]);

  if (model.days.length === 0) {
    return (
      <div className="flex h-40 items-center justify-center text-sm text-muted-foreground">
        No usage recorded in this window.
      </div>
    );
  }
  return <div className="w-full" ref={elRef} aria-label="usage by day, stacked by category" />;
}
