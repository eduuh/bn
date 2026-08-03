import { Link } from "react-router-dom";
import { ChevronRight } from "lucide-react";
import type { FleetEntry } from "../lib/fleet";
import type { HarnessOption } from "../api/types";
import { prHref, prLabel } from "../lib/board";
import { Age, FleetCounters } from "./FleetMeta";
import { HarnessMenu } from "./HarnessMenu";
import { cn } from "../lib/utils";

/** The left tone dot for a row's state — the same scale as the card's tone bar. */
const DOT: Record<string, string> = {
  green: "bg-ok",
  red: "bg-urgent",
  amber: "bg-warn",
  grey: "bg-border",
  neutral: "bg-border",
};

/**
 * A healthy agent as one scannable line.
 *
 * Nothing here needs a decision, so it doesn't need a card: the whole working fleet fits on one
 * screen instead of three, and the [`FleetEntryCard`] surface stays meaningful — a card on this
 * board now *means* something is waiting on you. Branch first (the only field that differs across a
 * single-repo fleet), then a state dot and the motion counters, all in one row that taps through to
 * the agent detail.
 *
 * The harness chip rides along at row scale; switching still works, it just no longer costs every
 * agent 90px of board.
 */
export function FleetRow({
  entry,
  now,
  harnesses,
}: {
  entry: FleetEntry;
  now: number;
  harnesses?: HarnessOption[];
}) {
  const to = entry.reviewId
    ? "/?tab=reviews"
    : entry.sessionId
      ? `/agents/${encodeURIComponent(entry.sessionId)}`
      : null;
  const href = prHref(entry.pr ?? undefined);

  const inner = (
    <>
      <span
        className={cn("h-4 w-1 shrink-0 rounded-full", DOT[entry.tone] ?? DOT.neutral)}
        aria-hidden="true"
      />
      <span className="min-w-0 flex-1 truncate font-mono text-sm text-foreground">{entry.branch}</span>

      {/* Fixed-width columns, not a right-packed pile: each field lands at the same x on every row,
          so the list reads down a column instead of forcing a re-scan per line. `sm:contents` keeps
          one DOM across the breakpoint — a second mobile copy would make screen readers say every
          counter twice. */}
      <span className="hidden sm:contents">
        <span className="w-24 shrink-0 truncate text-right text-xs text-muted-foreground">
          {entry.stateLabel}
        </span>
        <span className="flex w-24 shrink-0 justify-end">
          <HarnessMenu target={entry.target} current={entry.harness} harnesses={harnesses} />
        </span>
        <FleetCounters entry={entry} variant="columns" />
        <span className="hidden w-12 shrink-0 truncate text-right text-xs md:block">
          {entry.pr &&
            (href ? (
              <a
                href={href}
                target="_blank"
                rel="noreferrer"
                onClick={(e) => e.stopPropagation()}
                className="text-primary"
              >
                {prLabel(entry.pr)}
              </a>
            ) : (
              <span className="text-muted-foreground">{prLabel(entry.pr)}</span>
            ))}
        </span>
      </span>

      <span className="w-12 shrink-0 text-right">
        <Age ts={entry.updatedAt} now={now} />
      </span>
      <ChevronRight
        className={cn("h-4 w-4 shrink-0 text-muted-foreground/50", !to && "invisible")}
        aria-hidden="true"
      />
    </>
  );

  const surface =
    "flex items-center gap-2 rounded-md px-2 py-2 transition-colors hover:bg-accent/40";

  if (to) {
    return (
      <Link to={to} className={surface}>
        {inner}
      </Link>
    );
  }
  return <div className={surface}>{inner}</div>;
}
