// Small pure formatters for the dashboard. Kept dependency-free and side-effect-free so they read
// the same in the board and in unit tests.

/** A compact "42s / 3m / 2h 10m / 1d 4h" relative age. `now`/`ts` are epoch seconds. */
export function timeAgo(ts: number, now: number): string {
  if (!ts || ts <= 0) return "—";
  const secs = Math.max(now - ts, 0);
  if (secs < 60) return `${secs}s`;
  const mins = Math.floor(secs / 60);
  if (mins < 60) return `${mins}m`;
  const hrs = Math.floor(mins / 60);
  if (hrs < 24) return `${hrs}h ${mins % 60}m`;
  const days = Math.floor(hrs / 24);
  return `${days}d ${hrs % 24}h`;
}

/** Human token counts: `840 / 3.2k / 1.45M / 4.84B`. The billions tier is not hypothetical —
 * every turn of a cached agent session re-reads the whole prefix, so a month of cache-read totals
 * runs to 10^9 and would otherwise render as `4839.35M`. */
export function fmtTokens(n: number): string {
  if (!Number.isFinite(n)) return "—";
  if (n < 1_000) return String(n);
  if (n < 1_000_000) return `${(n / 1_000).toFixed(1)}k`;
  if (n < 1_000_000_000) return `${(n / 1_000_000).toFixed(2)}M`;
  return `${(n / 1_000_000_000).toFixed(2)}B`;
}

/** A merges-per-hour rate, e.g. `0.75/hr` or `2.4/hr`; `—` when zero/unknown. */
export function fmtRate(r: number): string {
  if (!Number.isFinite(r) || r <= 0) return "—";
  if (r < 1) return `${r.toFixed(2)}/hr`;
  return `${r.toFixed(1)}/hr`;
}

/** A rough ETA in the largest sensible unit; `done` at zero, `—` when incomputable. */
export function fmtEta(hours: number | null): string {
  if (hours === null || !Number.isFinite(hours)) return "—";
  if (hours <= 0) return "done";
  if (hours < 1) return `${Math.round(hours * 60)}m`;
  if (hours < 24) return `${hours.toFixed(1)}h`;
  return `${(hours / 24).toFixed(1)}d`;
}

/** A wall-clock `HH:MM` (local) for a recovery-event timestamp. */
export function fmtClock(ts: number): string {
  if (!ts || ts <= 0) return "--:--";
  const d = new Date(ts * 1000);
  const hh = String(d.getHours()).padStart(2, "0");
  const mm = String(d.getMinutes()).padStart(2, "0");
  return `${hh}:${mm}`;
}

/** 2dp with thousands grouping — `332,589.65`. Money is read at a glance on these screens, and an
 * ungrouped six-figure run of digits is the one number nobody can scan. */
function money(n: number): string {
  return n.toLocaleString("en-US", { minimumFractionDigits: 2, maximumFractionDigits: 2 });
}

/** AIC (the metered Copilot bill) — `—` when a group had no Copilot runs, else 2dp. Distinguishes
 * absent (null) from a real `0.00`. */
export function fmtAic(aic: number | null): string {
  if (aic === null || !Number.isFinite(aic)) return "—";
  return money(aic);
}

/** AIC at board density — `—`, `48.51`, `2.8k`, `147.4k`, `1.20M`.
 *
 * The same number as `fmtAic` at the precision a scan can use. Full 2dp is right on a detail screen
 * where the figure is the subject; in a list row it is up to ten characters of mostly-noise digits,
 * and the first thing a narrow card drops is the tail — which turned a six-figure spend into the
 * unreadable `147,…`. Shares the `k`/`M` shape with `fmtTokens` so one habit reads both. */
export function fmtAicCompact(aic: number | null): string {
  if (aic === null || !Number.isFinite(aic)) return "—";
  const n = Math.abs(aic);
  if (n < 1_000) return aic.toFixed(2);
  if (n < 1_000_000) return `${(aic / 1_000).toFixed(1)}k`;
  return `${(aic / 1_000_000).toFixed(2)}M`;
}

/** $ (the cache-aware API-equiv benchmark) — `—` when nothing in the group was priceable, else
 * `$1.23`. Never a bill; a benchmark. */
export function fmtUsd(usd: number | null): string {
  if (usd === null || !Number.isFinite(usd)) return "—";
  return `$${money(usd)}`;
}

/** Cache reads then writes — the `bn cost` CACHE r/w column, e.g. `3.90M / 412K`. The two stay
 * apart because they move the bill in opposite directions: a read bills at a tenth of fresh input,
 * a write at 1.25–2× it, so one number for "cache" would hide which kind of traffic a row is. */
export function fmtCacheReadWrite(read: number, write: number): string {
  return `${fmtTokens(read)} / ${fmtTokens(write)}`;
}
