import type { FleetEntry } from "../lib/fleet";
import { fmtTokens, timeAgo } from "../lib/format";
import { cn } from "../lib/utils";

/** The `·` between meta items. Decorative, so it's hidden from assistive tech. */
export function Dot() {
  return (
    <span className="select-none text-muted-foreground/40" aria-hidden="true">
      ·
    </span>
  );
}

/**
 * The motion counters on a fleet surface: turns, tools, tokens.
 *
 * The board used to show output tokens alone, which says how much an agent has *spent*, not whether
 * it is still moving — 937k tokens looks identical on an agent mid-edit and on one wedged an hour
 * ago. Turns and tool calls are the progress signal.
 *
 * Two layouts, because a card and a list want opposite things from the same numbers:
 *
 * - `inline` (cards) — dot-separated and shrink-wrapped. Each item leads with its own [`Dot`], so a
 *   surface can drop the whole component behind a breakpoint without stranding a separator. A
 *   counter an older server didn't send simply doesn't render.
 * - `columns` (rows) — fixed-width and right-aligned, so the numbers line up down the list and a
 *   column can be read as a column. A missing counter holds its slot rather than shifting the row,
 *   and the separators go away: alignment already does that job, and doing both is noise.
 */
export function FleetCounters({
  entry,
  variant = "inline",
}: {
  entry: FleetEntry;
  variant?: "inline" | "columns";
}) {
  const turns = entry.turns ? `${entry.turns} turns` : "";
  const tools = entry.toolCalls ? `${entry.toolCalls} tools` : "";
  const tokens = entry.outputTokens ? `${fmtTokens(entry.outputTokens)} tok` : "";

  if (variant === "columns") {
    const col = "shrink-0 truncate text-right text-xs tabular-nums text-muted-foreground";
    return (
      <>
        <span className={cn(col, "w-20")}>{turns}</span>
        {/* Tools is the first thing to go when the row gets tight — turns and tokens carry the
            same signal more cheaply. */}
        <span className={cn(col, "hidden w-20 lg:block")}>{tools}</span>
        <span className={cn(col, "w-24")}>{tokens}</span>
      </>
    );
  }

  return (
    <>
      {[turns, tools, tokens].filter(Boolean).map((t) => (
        <span key={t} className="contents">
          <Dot />
          <span className="whitespace-nowrap text-xs tabular-nums text-muted-foreground">{t}</span>
        </span>
      ))}
    </>
  );
}

/**
 * Time since the agent last moved — the triage signal, and the one meta item that survives every
 * breakpoint. Dimmer than the counters so it reads as context rather than another number.
 */
export function Age({ ts, now }: { ts: number; now: number }) {
  return (
    <span className="whitespace-nowrap text-xs tabular-nums text-muted-foreground/70">
      {timeAgo(ts, now)}
    </span>
  );
}
