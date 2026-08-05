// Pure cost-view logic: the work-class palette and the (day × category) cross-tab → stacked-bar
// model. Side-effect-free and DOM-free so the chart stays a thin render of these values and the
// stacking math is unit-tested (`cost.test.ts`).

import type { DayCategory, WorkClass } from "../api/types";

/** The metric a chart/row can stack or sort by. Output tokens is the always-present usage signal
 * (AIC/$ are `null` for non-Copilot / unpriceable groups), so it's the chart default. */
export type CostMetric = "output_tokens" | "aic" | "cost_usd";

/** Work-class display order (bottom-of-stack first) — mirrors the `bn cost` category ordering. */
export const CATEGORY_ORDER: WorkClass[] = [
  "orchestration",
  "coding",
  "scheduled",
  "research",
  "remote",
  "unattributed",
];

/** Label + colour per known work-class (title-case labels match the CLI's `WorkClass::label`). */
const CATEGORY_META: Record<string, { label: string; color: string }> = {
  orchestration: { label: "Orchestration", color: "#a371f7" },
  coding: { label: "Coding", color: "#3fb950" },
  scheduled: { label: "Scheduled", color: "#d29922" },
  research: { label: "Research", color: "#58a6ff" },
  remote: { label: "Remote", color: "#db6d28" },
  unattributed: { label: "Unattributed", color: "#6e7681" },
};

/** Colours for any category not in the known table, so an added work-class never crashes the UI. */
const FALLBACK_COLORS = ["#ec6547", "#79c0ff", "#e3b341", "#56d364", "#ff7b72", "#d2a8ff"];

/** The human label for a category key (title-case for known classes, else the raw key). */
export function categoryLabel(cat: string): string {
  return CATEGORY_META[cat]?.label ?? cat;
}

/** The bar/legend colour for a category key; unknown keys cycle the fallback palette by index. */
export function categoryColor(cat: string, fallbackIdx = 0): string {
  return CATEGORY_META[cat]?.color ?? FALLBACK_COLORS[fallbackIdx % FALLBACK_COLORS.length];
}

/** The categories actually present in the cross-tab, in display order (known classes first in
 * [`CATEGORY_ORDER`], then any unknown ones alphabetically) — the chart's series + legend order. */
export function presentCategories(cells: DayCategory[]): string[] {
  const seen = new Set(cells.map((c) => c.category));
  const known = CATEGORY_ORDER.filter((c) => seen.has(c));
  const extra = [...seen].filter((c) => !CATEGORY_ORDER.includes(c as WorkClass)).sort();
  return [...known, ...extra];
}

/** A metric value off a cross-tab cell, treating a `null` cost as 0 so stacking math is total. */
function cellValue(cell: DayCategory, metric: CostMetric): number {
  const v = cell[metric];
  return typeof v === "number" && Number.isFinite(v) ? v : 0;
}

/** The stacked-bar source model: sorted day labels (x), the present categories (series order), and
 * a `values[categoryIdx][dayIdx]` matrix of the chosen metric. Pure + deterministic. */
export interface StackModel {
  days: string[];
  categories: string[];
  /** values[categoryIdx][dayIdx] — the metric contributed by that category on that day. */
  values: number[][];
}

/** Shape the (day × category) cross-tab into a [`StackModel`] for the chosen metric. */
export function buildStackModel(cells: DayCategory[], metric: CostMetric): StackModel {
  const days = [...new Set(cells.map((c) => c.day))].sort();
  const categories = presentCategories(cells);
  const dayIdx = new Map(days.map((d, i) => [d, i]));
  const catIdx = new Map(categories.map((c, i) => [c, i]));
  const values = categories.map(() => new Array<number>(days.length).fill(0));
  for (const cell of cells) {
    const di = dayIdx.get(cell.day);
    const ci = catIdx.get(cell.category);
    if (di === undefined || ci === undefined) continue;
    values[ci][di] += cellValue(cell, metric);
  }
  return { days, categories, values };
}

/** Cumulative-stacked series ready for uPlot bars. Each series' plotted y is the running top of the
 * stack (category 0 at the bottom); they're returned in *draw order* — tallest cumulative first —
 * because uPlot paints later series on top, so drawing the shortest (bottom) band last leaves each
 * category's slice visible. `order[k]` is the category index that draw-series `k` colours. */
export interface StackedSeries {
  order: number[];
  /** cumulative[drawSeriesIdx][dayIdx] — the running stack top for that draw series. */
  cumulative: number[][];
}

export function stackForDraw(values: number[][]): StackedSeries {
  const nCats = values.length;
  const nDays = values[0]?.length ?? 0;
  // Running total up to and including each category, in display order.
  const cum: number[][] = values.map(() => new Array<number>(nDays).fill(0));
  for (let d = 0; d < nDays; d++) {
    let acc = 0;
    for (let c = 0; c < nCats; c++) {
      acc += values[c][d];
      cum[c][d] = acc;
    }
  }
  // Draw the highest cumulative (last category) first so the first category paints last, on top.
  const order: number[] = [];
  const cumulative: number[][] = [];
  for (let c = nCats - 1; c >= 0; c--) {
    order.push(c);
    cumulative.push(cum[c]);
  }
  return { order, cumulative };
}
