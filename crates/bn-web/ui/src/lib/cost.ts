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

/**
 * Label + colour per known work-class (title-case labels match the CLI's `WorkClass::label`).
 *
 * The colours used to be GitHub Primer's defaults (`#a371f7`, `#3fb950`, `#58a6ff`, and the
 * blue-grey `#6e7681`), left over from before the surface moved to the warm clay system. That put
 * the app's one chart in a different palette from everything around it — and Primer's greys are
 * blue-grey, which is the exact thing `tailwind.css` opens by rejecting.
 *
 * These are the validated categorical steps for a dark surface, assigned **in fixed slot order** —
 * the order is what makes the set colourblind-safe, so it must never be shuffled or cycled. Checked
 * against this app's actual card surface (`#242321`, i.e. `--card`) with the palette validator:
 * lightness band, chroma floor, protan/deutan separation (worst adjacent ΔE 8.4), normal-vision
 * floor (19.3) and ≥3:1 contrast all pass.
 *
 * `unattributed` is deliberately *not* a categorical slot: it is the residual bucket, so it takes a
 * warm neutral (3.56:1 on the card) and spends no hue on identity it doesn't have.
 */
const CATEGORY_META: Record<string, { label: string; color: string }> = {
  orchestration: { label: "Orchestration", color: "#3987e5" }, // slot 1 · blue
  coding: { label: "Coding", color: "#d95926" }, //              slot 2 · orange
  scheduled: { label: "Scheduled", color: "#199e70" }, //        slot 3 · aqua
  research: { label: "Research", color: "#c98500" }, //          slot 4 · yellow
  remote: { label: "Remote", color: "#d55181" }, //              slot 5 · magenta
  unattributed: { label: "Unattributed", color: "#7d7869" }, //  residual · warm neutral
};

/** Slots 6–8 of the same validated set, for any work-class not in the table above, so an added
 * category never crashes the UI and never invents an unvalidated hue. */
const FALLBACK_COLORS = ["#008300", "#9085e9", "#e66767"];

/** The human label for a category key (title-case for known classes, else the raw key). */
export function categoryLabel(cat: string): string {
  return CATEGORY_META[cat]?.label ?? cat;
}

/** The product name for a harness key — mirrors `bn_core::cost::harness_label`, so the web tab and
 * `bn cost --by harness` name the two the same way. Unknown harnesses render as their raw token and
 * the empty one (the scheduled ledger records no harness) as an em dash. */
export function harnessLabel(harness: string): string {
  if (harness === "claude") return "Claude Code";
  if (harness === "copilot") return "Copilot";
  return harness === "" || harness === "—" ? "—" : harness;
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
