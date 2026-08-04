import { cn } from "../lib/utils";

/** Value tint per tone — the dashboard's ok/urgent/info/warn signal scale, grey → plain text. */
const TONE: Record<string, string> = {
  green: "text-ok",
  red: "text-urgent",
  blue: "text-info",
  amber: "text-warn",
  grey: "text-foreground",
};

/**
 * A row of stat tiles on a fixed column grid.
 *
 * The tiles used to sit in `flex flex-wrap` with `flex-1` on each tile, which sizes every *line*
 * independently: a row of six and a row of four resolved to different tile widths, so Insights read
 * as two unrelated ragged bands. A grid gives one column rhythm no matter how many tiles a page
 * happens to have, and `auto-rows-fr` keeps them equal height when a label wraps.
 */
export function StatGrid({
  children,
  cols = 4,
  className,
}: {
  children: React.ReactNode;
  /**
   * Columns at the widest breakpoint. Tiles start 2-up on a phone and widen from there — except
   * `cols={2}`, which stays 2-up at every size because it is used inside a narrow side column,
   * where the viewport being wide says nothing about the space the tiles actually have.
   */
  cols?: 2 | 3 | 4 | 5 | 6;
  className?: string;
}) {
  return (
    <div
      className={cn(
        "grid auto-rows-fr grid-cols-2 gap-px bg-border",
        cols !== 2 && "sm:grid-cols-3",
        cols === 3 && "lg:grid-cols-3",
        cols === 4 && "lg:grid-cols-4",
        cols === 5 && "lg:grid-cols-5",
        cols === 6 && "lg:grid-cols-6",
        className,
      )}
    >
      {children}
    </div>
  );
}

/**
 * A single dashboard stat tile. Inside a `StatGrid` the tiles are separated by the grid's 1px
 * background showing through rather than by gaps between floating cards — the same rule-not-gap
 * treatment the panes use, so a stat band reads as one object.
 */
export function StatTile({
  label,
  value,
  tone = "grey",
  title,
  sub,
}: {
  label: string;
  value: string;
  tone?: "green" | "red" | "blue" | "amber" | "grey";
  title?: string;
  /** Optional second line — a comparison, a unit, a freshness stamp. */
  sub?: React.ReactNode;
}) {
  return (
    <div className="flex flex-col justify-center gap-1 bg-card px-3.5 py-3" title={title}>
      <div
        className={cn(
          "font-mono text-[1.35rem] font-semibold leading-none tabular-nums",
          TONE[tone],
        )}
      >
        {value}
      </div>
      <div className="text-[0.68rem] uppercase tracking-[0.08em] text-muted-foreground">{label}</div>
      {sub && <div className="text-[0.7rem] text-muted-foreground/70">{sub}</div>}
    </div>
  );
}
