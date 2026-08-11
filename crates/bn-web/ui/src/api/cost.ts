// The cost surface (GET /api/cost .data; mirrors `bn_core::cost::CostView` from #381).
//
// Split out of `types.ts` when cache-write accounting landed: the row shapes gained a bucket, and
// the file they lived in was already at its size ratchet. They are re-exported from `types.ts`, so
// every existing `import type { CostRow } from "../api/types"` keeps working and new code can
// import from either.

/** The work-class category token on a cost row (`bn_core::cost::WorkClass::as_str`). Rows recorded
 * before #381 threaded a class through fold into `unattributed` (expected to dominate history). */
export type WorkClass =
  "orchestration" | "coding" | "scheduled" | "research" | "remote" | "unattributed";

/** One grouped cost line — `bn_core::cost::CostRow`. `aic` (the *metered Copilot bill*) is `null`
 * for a group with no Copilot runs; `cost_usd` (the *API-equiv benchmark*) is `null` when nothing
 * in the group had a priceable model. */
export interface CostRow {
  key: string;
  /** Fresh (non-cached) input tokens — on a Claude Code run, usually the smallest of the three
   * prompt buckets and never the whole prompt. */
  input_tokens: number;
  /** Cached prompt tokens (priced ~10× cheaper than fresh input). */
  cache_read_tokens: number;
  /** Prompt tokens written *into* the cache, both TTLs together. Priced at 1.25–2× fresh input on
   * Anthropic, so this column is spend rather than savings. Zero on every run recorded before the
   * bucket was captured — "never measured", not "none written". */
  cache_write_tokens: number;
  output_tokens: number;
  sessions: number;
  /** AIC — the real Copilot bill. */
  aic: number | null;
  /** $ — the cache-aware API-equiv benchmark. */
  cost_usd: number | null;
}

/** Whole-window totals in both cost units — `bn_core::cost::CostTotals`. */
export interface CostTotals {
  sessions: number;
  input_tokens: number;
  cache_read_tokens: number;
  cache_write_tokens: number;
  output_tokens: number;
  aic: number | null;
  cost_usd: number | null;
}

/** One (day × category) cell — `bn_core::cost::DayCategory`, the stacked-bar chart's source. */
export interface DayCategory {
  day: string;
  category: string;
  sessions: number;
  input_tokens: number;
  cache_read_tokens: number;
  cache_write_tokens: number;
  output_tokens: number;
  aic: number | null;
  cost_usd: number | null;
}

/** GET /api/cost .data — `bn_core::cost::CostView`. Every breakdown is always present, so the page
 * switches the table dimension client-side; only the window picker changes the fetch. */
export interface CostView {
  window_days: number;
  totals: CostTotals;
  by_category: CostRow[];
  by_machine: CostRow[];
  by_repo: CostRow[];
  by_model: CostRow[];
  /** Claude Code vs Copilot. Read this fold down `cost_usd`, not `aic`: AIC is metered by Copilot
   * and does not exist for Claude, so a Claude row's null renders `—` and reads, wrongly, as free.
   * The $ benchmark is the only figure computed the same way for both. */
  by_harness: CostRow[];
  by_day: CostRow[];
  by_day_category: DayCategory[];
}
