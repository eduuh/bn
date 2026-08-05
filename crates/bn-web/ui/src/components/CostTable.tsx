import type { CostRow, CostView } from "../api/types";
import { fmtAic, fmtInputCache, fmtTokens, fmtUsd } from "../lib/format";
import { categoryLabel } from "../lib/cost";
import { Tabs, TabsList, TabsTrigger } from "./ui/tabs";

/** The breakdown dimensions the `by` switcher offers — the five [`CostView`] `by_*` folds. */
export type CostDimension = "category" | "machine" | "repo" | "model" | "day";

const DIMENSIONS: { key: CostDimension; label: string }[] = [
  { key: "category", label: "Category" },
  { key: "machine", label: "Machine" },
  { key: "repo", label: "Repo" },
  { key: "model", label: "Model" },
  { key: "day", label: "Day" },
];

function rowsFor(view: CostView, dim: CostDimension): CostRow[] {
  switch (dim) {
    case "category":
      return view.by_category;
    case "machine":
      return view.by_machine;
    case "repo":
      return view.by_repo;
    case "model":
      return view.by_model;
    case "day":
      return view.by_day;
  }
}

/** Categories render title-case; every other dimension shows its raw key (repo / model id / etc). */
function keyLabel(dim: CostDimension, key: string): string {
  return dim === "category" ? categoryLabel(key) : key;
}

/**
 * The breakdown table (design §4(b)) with a `by` switcher over the five folds. Columns mirror
 * `bn cost` exactly — SESS · INPUT(cache) · OUTPUT · AIC · $ (API-eq) — with a totals footer, so
 * the web view and the CLI read identically. The table scrolls horizontally on narrow screens so
 * the columns never clip at 375px (#547).
 */
export function CostTable({
  view,
  by,
  onBy,
}: {
  view: CostView;
  by: CostDimension;
  onBy: (dim: CostDimension) => void;
}) {
  const rows = rowsFor(view, by);
  const t = view.totals;
  const header = DIMENSIONS.find((d) => d.key === by)?.label ?? "Key";

  return (
    <div className="space-y-3">
      <Tabs value={by} onValueChange={(v) => onBy(v as CostDimension)}>
        <TabsList className="flex w-full overflow-x-auto sm:w-auto sm:inline-flex" aria-label="breakdown dimension">
          {DIMENSIONS.map((d) => (
            <TabsTrigger key={d.key} value={d.key} className="flex-1 sm:flex-none">
              {d.label}
            </TabsTrigger>
          ))}
        </TabsList>
      </Tabs>

      <div className="-mx-4 overflow-x-auto px-4 sm:mx-0 sm:px-0">
        <table className="w-full min-w-[32rem] border-collapse text-sm">
          <thead>
            <tr className="border-b border-border text-xs uppercase tracking-wide text-muted-foreground">
              <th className="py-2 pr-3 text-left font-medium">{header}</th>
              <th className="py-2 pl-3 text-right font-medium">Sess</th>
              <th className="py-2 pl-3 text-right font-medium">Input (cache)</th>
              <th className="py-2 pl-3 text-right font-medium">Output</th>
              <th className="py-2 pl-3 text-right font-medium">AIC</th>
              <th className="py-2 pl-3 text-right font-medium">$ (API-eq)</th>
            </tr>
          </thead>
          <tbody>
            {rows.length === 0 ? (
              <tr>
                <td className="py-6 text-center text-muted-foreground" colSpan={6}>
                  No recorded runs in this window.
                </td>
              </tr>
            ) : (
              rows.map((r) => (
                <tr key={r.key} className="border-b border-border/60 last:border-0">
                  <td className="py-2 pr-3 font-mono text-foreground">{keyLabel(by, r.key)}</td>
                  <td className="py-2 pl-3 text-right tabular-nums">{r.sessions}</td>
                  <td className="py-2 pl-3 text-right font-mono tabular-nums text-muted-foreground">
                    {fmtInputCache(r.input_tokens, r.cache_read_tokens)}
                  </td>
                  <td className="py-2 pl-3 text-right font-mono tabular-nums">{fmtTokens(r.output_tokens)}</td>
                  <td className="py-2 pl-3 text-right font-mono tabular-nums text-ok">{fmtAic(r.aic)}</td>
                  <td className="py-2 pl-3 text-right font-mono tabular-nums text-info">{fmtUsd(r.cost_usd)}</td>
                </tr>
              ))
            )}
          </tbody>
          {rows.length > 0 && (
            <tfoot>
              <tr className="border-t border-border font-semibold">
                <td className="py-2 pr-3">Total</td>
                <td className="py-2 pl-3 text-right tabular-nums">{t.sessions}</td>
                <td className="py-2 pl-3 text-right font-mono tabular-nums text-muted-foreground">
                  {fmtInputCache(t.input_tokens, t.cache_read_tokens)}
                </td>
                <td className="py-2 pl-3 text-right font-mono tabular-nums">{fmtTokens(t.output_tokens)}</td>
                <td className="py-2 pl-3 text-right font-mono tabular-nums text-ok">{fmtAic(t.aic)}</td>
                <td className="py-2 pl-3 text-right font-mono tabular-nums text-info">{fmtUsd(t.cost_usd)}</td>
              </tr>
            </tfoot>
          )}
        </table>
      </div>
    </div>
  );
}
