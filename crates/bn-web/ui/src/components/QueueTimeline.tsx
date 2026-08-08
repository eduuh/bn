import { useMemo } from "react";

import type { QueueTimelineView, TimelineMark } from "../api/queue-types";
import { cn } from "../lib/utils";

/**
 * The day's work queue, drawn on an hour axis.
 *
 * **One row per piece of work**, grouped under its band. The first version drew one row per *band*
 * — four 14-unit rows in a 1440-unit-wide viewBox — so the whole day rendered about twenty pixels
 * tall, every item became an unlabelled tick, and a minutely cron turned the Scheduled band into a
 * solid bar. It was a decoration sitting above the lists that carried the actual information, which
 * is the opposite of what a timeline is for: the point is to see *when*, and against what else.
 *
 * Bands stay as grouping because "running" and "merely projected" are different claims:
 *
 *   Running   — alive now, a span from its start to the current time
 *   Scheduled — cron and Markdown agents at their declared `on_calendar` slots
 *   Projected — queued work with no clock, placed by replaying the dispatcher forward
 *   Done      — finished today
 *
 * **Inline SVG, no chart library** — the dashboard ships inside the bn binary and is served over a
 * LAN that often has no route to the internet, so a CDN import fails silently.
 *
 * x is minutes-since-midnight and nothing else; the label gutter is carved out with a negative
 * viewBox origin rather than by offsetting every mark, so the component needs no layout maths and
 * is correct at any width.
 *
 * Three honesty rules the drawing enforces:
 *
 *  - A **projected** mark is hollow and dashed. It is a guess from an estimated duration; drawing it
 *    like a scheduled time would be a lie told every time the guess is wrong.
 *  - A **repeating** job collapses to one row carrying its count. `queue-tick` fires 1440 times a
 *    day; drawing all of them is not more information, it is less. The count keeps it honest —
 *    silently dropping the repeats would understate the day.
 *  - Work that does not fit is **counted, not drawn**, rather than crammed against the right edge.
 */
export function QueueTimeline({ view }: { view: QueueTimelineView }) {
  const rows = useMemo(() => buildRows(view), [view]);

  if (rows.length === 0) {
    return (
      <p className="text-sm text-muted-foreground">
        Nothing ran, is running, or is queued for {view.date}.
      </p>
    );
  }

  const height = TOP + rows.length * ROW + 12;
  const nowX = minutesInto(view.now, view.start);

  return (
    <section aria-label="queue timeline" className="space-y-2">
      <div className="overflow-x-auto">
        <svg
          viewBox={`${-GUTTER} 0 ${1440 + GUTTER + 16} ${height}`}
          className="h-auto w-full min-w-[40rem]"
          role="img"
          aria-label={`Queue timeline for ${view.date}`}
        >
          {HOURS.map((h) => (
            <line
              key={h}
              x1={h * 60}
              x2={h * 60}
              y1={TOP - 10}
              y2={height - 12}
              className={cn("stroke-border", h % 6 === 0 ? "opacity-60" : "opacity-20")}
              strokeWidth={1}
            />
          ))}
          {[0, 3, 6, 9, 12, 15, 18, 21].map((h) => (
            <text key={h} x={h * 60 + 4} y={TOP - 14} className="fill-muted-foreground text-[18px]">
              {String(h).padStart(2, "0")}
            </text>
          ))}

          {rows.map((row, i) =>
            row.kind === "header" ? (
              <text
                key={`h-${row.label}-${i}`}
                x={-GUTTER + 6}
                y={TOP + i * ROW + ROW * 0.7}
                className="fill-foreground/70 text-[19px] font-semibold uppercase"
              >
                {row.label}
              </text>
            ) : (
              <MarkRow key={row.mark.key} row={row} y={TOP + i * ROW} start={view.start} />
            ),
          )}

          {/* The current-time rule, from server truth rather than the browser clock. */}
          <line
            x1={nowX}
            x2={nowX}
            y1={TOP - 18}
            y2={height - 12}
            className="stroke-foreground/50"
            strokeWidth={2}
          />
          <text x={nowX + 6} y={TOP - 24} className="fill-foreground/70 text-[17px]">
            now
          </text>
        </svg>
      </div>

      {view.beyond > 0 && (
        <p className="text-xs text-muted-foreground">
          {view.beyond} more queued than fits today at projected throughput.
        </p>
      )}
    </section>
  );
}

/** One drawn row: a band heading, or a piece of work. */
type Row =
  | { kind: "header"; label: string }
  | {
      kind: "mark";
      mark: TimelineMark;
      repeats: number;
      band: string;
      /** For a collapsed recurrence, the span its occurrences actually cover. */
      from: number;
      to: number;
    };

const GUTTER = 300;
const ROW = 34;
const TOP = 40;
/** Rows drawn before the rest become a count. Enough for a real day, short of a wall. */
const MAX_ROWS = 26;
const HOURS = Array.from({ length: 24 }, (_, i) => i);

/**
 * Flatten the bands into drawable rows, collapsing repeats.
 *
 * A job that fires every minute contributes 1440 marks to one band. Collapsing by label keeps the
 * row it deserves and carries the repeat count, so the day is neither a wall of identical ticks nor
 * quietly understated.
 */
function buildRows(view: QueueTimelineView): Row[] {
  const out: Row[] = [];
  for (const band of view.bands) {
    if (band.marks.length === 0) continue;
    const byLabel = new Map<
      string,
      { mark: TimelineMark; repeats: number; from: number; to: number }
    >();
    for (const mark of [...band.marks].sort((a, b) => a.at - b.at)) {
      const seen = byLabel.get(mark.label);
      if (seen) {
        seen.repeats += 1;
        // Carry the extent, not just the first firing. Collapsing to the earliest occurrence drew
        // `watch ×720` as a dot at midnight — which reads as "ran once, at 00:00" when the truth is
        // "runs all day". The row is one row either way; the bar is what makes it honest.
        seen.to = Math.max(seen.to, mark.end, mark.at);
      } else {
        byLabel.set(mark.label, {
          mark,
          repeats: 0,
          from: mark.at,
          to: Math.max(mark.end, mark.at),
        });
      }
    }
    const marks = [...byLabel.values()];
    if (marks.length === 0) continue;
    out.push({ kind: "header", label: band.label });
    for (const m of marks) {
      out.push({
        kind: "mark",
        mark: m.mark,
        repeats: m.repeats,
        band: band.id,
        from: m.from,
        to: m.to,
      });
    }
  }
  if (out.length <= MAX_ROWS) return out;
  return out.slice(0, MAX_ROWS);
}

function MarkRow({
  row,
  y,
  start,
}: {
  row: Extract<Row, { kind: "mark" }>;
  y: number;
  start: number;
}) {
  const { mark, repeats } = row;
  const x = minutesInto(row.from, start);
  // A point mark (scheduled/projected) has end == at. Floor the width so a real thing is never
  // drawn as nothing, and so a 20-second job is still clickable.
  const w = Math.max(minutesInto(row.to, start) - x, 8);
  // A recurrence is drawn as its span, lightened: the bar says *when this job is active today*,
  // and the lightening says it is a range of occurrences rather than one long run.
  const recurring = repeats > 0;
  const tone = KIND_TONE[mark.kind] ?? "text-muted-foreground";
  const title = `${mark.label} — ${mark.kind}${mark.node ? ` on ${mark.node}` : ""}${
    repeats > 0 ? `\n${repeats + 1} times today` : ""
  }${mark.note ? `\n${mark.note}` : ""}`;

  return (
    <g className={tone}>
      <title>{title}</title>
      {/* The row's own faint baseline, so the eye can follow a label across to its block. */}
      <line
        x1={0}
        x2={1440}
        y1={y + ROW / 2}
        y2={y + ROW / 2}
        className="stroke-border opacity-30"
        strokeWidth={1}
      />
      <text
        x={-GUTTER + 14}
        y={y + ROW / 2 + 6}
        className="fill-foreground/85 text-[18px]"
        textLength={mark.label.length > 30 ? GUTTER - 70 : undefined}
        lengthAdjust="spacingAndGlyphs"
      >
        {mark.label}
      </text>
      {repeats > 0 && (
        <text x={-34} y={y + ROW / 2 + 6} className="fill-muted-foreground text-[15px]">
          ×{repeats + 1}
        </text>
      )}
      <rect
        x={x}
        y={y + ROW / 2 - 8}
        width={w}
        height={16}
        rx={4}
        fill={mark.projected ? "none" : "currentColor"}
        stroke="currentColor"
        strokeWidth={mark.projected ? 2 : 0}
        strokeDasharray={mark.projected ? "6 4" : undefined}
        opacity={recurring ? 0.4 : 1}
      />
    </g>
  );
}

/** Minutes from the window start — the x coordinate, since the viewBox is one unit per minute. */
function minutesInto(ts: number, start: number): number {
  return Math.max(0, Math.min(1440, Math.round((ts - start) / 60)));
}

/**
 * Kind → semantic token. Tailwind tokens rather than hex, so light and dark come free — the same
 * reason `PipelineFlow` uses them.
 */
const KIND_TONE: Record<string, string> = {
  ship: "text-primary",
  enhance: "text-ok",
  "review-pr": "text-destructive",
  investigate: "text-info",
  research: "text-warn",
  scout: "text-warn",
  cron: "text-muted-foreground",
};
