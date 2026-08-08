import { Link } from "react-router-dom";
import { ChevronRight } from "lucide-react";
import type { FleetEntry } from "../lib/fleet";
import type { HarnessOption } from "../api/types";
import { prHref, prLabel } from "../lib/board";
import { Age, FleetCounters } from "./FleetMeta";
import { HarnessMenu } from "./HarnessMenu";
import { TaskChip } from "./TaskChip";
import { PlanStatusBadge } from "./PlanArtifacts";
import { Badge } from "./ui/badge";
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
  onSelect,
  selected,
  compact,
}: {
  entry: FleetEntry;
  now: number;
  harnesses?: HarnessOption[];
  /**
   * When the board is a master/detail (the `/fleet` two-column layout), a row *selects* rather than
   * navigates — the transcript opens in the pane beside it, so you can read one agent without
   * losing the list of the others. Without this the row keeps its original link behaviour.
   */
  onSelect?: () => void;
  selected?: boolean;
  /**
   * Drop the fixed-width metric columns. They are sized for a full-width board; in the `/fleet`
   * master column (26rem) their combined width exceeds the row, so `flex-1` squeezed the branch
   * name — the one field that identifies the row — down to nothing. Media queries can't see this:
   * the viewport is wide, the *column* is not.
   */
  compact?: boolean;
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
      <span className="min-w-0 flex-1 truncate font-mono text-sm text-foreground">
        {entry.branch}
      </span>
      {entry.workType === "setup" && (
        <Badge variant="info" className="shrink-0">
          setup
        </Badge>
      )}

      {/* Fixed-width columns, not a right-packed pile: each field lands at the same x on every row,
          so the list reads down a column instead of forcing a re-scan per line. `sm:contents` keeps
          one DOM across the breakpoint — a second mobile copy would make screen readers say every
          counter twice. */}
      {compact ? (
        <span className="shrink-0 text-xs text-muted-foreground">{entry.stateLabel}</span>
      ) : (
        <span className="hidden sm:contents">
          {/* What it's working on, before what it is: the task is the thing you scan a fleet for.
            Rows render nothing when unlinked — an extra muted word per line is noise at this
            density; the card variant says "no task" instead. */}
          <span className="flex w-40 shrink-0 justify-start">
            {entry.task && <TaskChip task={entry.task} className="max-w-full" />}
          </span>
          <span className="flex w-28 shrink-0 justify-start">
            {entry.plan && <PlanStatusBadge status={entry.plan.status} />}
          </span>
          <span className="w-24 shrink-0 truncate text-right text-xs text-muted-foreground">
            {entry.stateLabel}
          </span>
          {/* `relative z-10`: in select mode the row's hit area is a stretched button *behind* the
            content, so the controls that live inside the row have to sit above it to stay clickable. */}
          <span className="relative z-10 flex w-24 shrink-0 justify-end">
            <HarnessMenu target={entry.target} current={entry.harness} harnesses={harnesses} />
          </span>
          <FleetCounters entry={entry} variant="columns" />
          <span className="relative z-10 hidden w-12 shrink-0 truncate text-right text-xs md:block">
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
      )}

      <span className="w-12 shrink-0 text-right">
        <Age ts={entry.updatedAt} now={now} />
      </span>
      <ChevronRight
        className={cn("h-4 w-4 shrink-0 text-muted-foreground/50", !to && "invisible")}
        aria-hidden="true"
      />
    </>
  );

  const surface = cn(
    "flex w-full items-center gap-2 rounded-md px-2 py-2 text-left transition-colors hover:bg-accent/40",
    selected && "bg-accent/60",
  );

  // Select mode uses a stretched overlay button rather than wrapping the row, because the row
  // already contains interactive controls (the harness menu, the PR link) and a button inside a
  // button is invalid DOM — React warns, and the inner control stops being reachable.
  if (onSelect) {
    return (
      <div className={cn(surface, "relative")}>
        <button
          type="button"
          onClick={onSelect}
          aria-current={selected}
          aria-label={`Watch ${entry.branch}`}
          className="absolute inset-0 z-0 rounded-md focus-visible:outline focus-visible:outline-2 focus-visible:outline-ring"
        />
        {inner}
      </div>
    );
  }
  if (to) {
    return (
      <Link to={to} className={surface}>
        {inner}
      </Link>
    );
  }
  return <div className={surface}>{inner}</div>;
}
