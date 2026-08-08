import { Link } from "react-router-dom";
import { AlertTriangle, ChevronRight } from "lucide-react";
import type { FleetEntry } from "../lib/fleet";
import type { HarnessOption } from "../api/types";
import { timeAgo } from "../lib/format";
import { prHref, prLabel, jumpCommand } from "../lib/board";
import { CopyButton } from "./CopyButton";
import { Age, Dot, FleetCounters } from "./FleetMeta";
import { HarnessMenu } from "./HarnessMenu";
import { RelaunchButton } from "./RelaunchButton";
import { NoTaskChip, TaskChip } from "./TaskChip";
import { Badge } from "./ui/badge";
import { Card } from "./ui/card";
import { toneVariant } from "../lib/tone";
import { cn } from "../lib/utils";

/** The left tone bar colour for a card's state. */
const BAR: Record<string, string> = {
  green: "bg-ok",
  red: "bg-urgent",
  amber: "bg-warn",
  grey: "bg-border",
  neutral: "bg-border",
};

/**
 * The full-attention Fleet card — the form reserved for agents that need you and for orchestrators.
 * Everything healthy renders as a [`FleetRow`] instead, so this surface is spent only where a
 * decision is waiting.
 *
 * Hierarchy is branch-first (#479 rev.2): on a single-repo fleet the repo is identical on every
 * card, so the branch is the headline and the repo joins the meta line. Under it, the one-line
 * state, the reason you're being asked, the motion counters (turns · tools · tokens · age) that say
 * whether it's actually progressing, and the two actions worth taking from a board — relaunch, and
 * copy the tmux jump. The harness is a chip that opens [`HarnessMenu`]; it used to be a permanent
 * segmented block on every card, which spent the most valuable pixels on the rarest action.
 *
 * Taps through to the agent detail when a live session backs it, or to the Reviews inbox when it's
 * paused for plan-review; otherwise it stays a static glance card. The left border colours by state
 * so the board scans without reading.
 */
export function FleetEntryCard({
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
  const isOrchestrator = entry.role === "orchestrator";
  const age = timeAgo(entry.updatedAt, now);

  const inner = (
    <>
      {/* The card can't clip (the harness menu has to escape it), so the bar rounds itself. */}
      <span
        className={cn("absolute inset-y-0 left-0 w-1 rounded-l-lg", BAR[entry.tone] ?? BAR.neutral)}
        aria-hidden="true"
      />

      <div className="flex items-center gap-2">
        <span className="truncate font-mono text-sm font-semibold text-foreground">
          {entry.branch}
        </span>
        {isOrchestrator && (
          <Badge variant="info" className="shrink-0 gap-1">
            ◆ Orchestrator
          </Badge>
        )}
        {entry.workType === "setup" && (
          <Badge variant="info" className="shrink-0">
            Repo setup
          </Badge>
        )}
        <Badge variant={toneVariant(entry.tone)} className="shrink-0">
          {entry.statusLabel}
        </Badge>
        <span className="flex-1" />
        {to && <ChevronRight className="h-4 w-4 shrink-0 text-muted-foreground" />}
      </div>

      {/* One fact, one encoding. A stuck agent used to say it three times — the `stuck` badge, a
          "Stuck" line, and the "No progress for 21h" banner — so the two lines that carried no new
          information crowded out the one that did. The banner supersedes the state line whenever it
          renders, and the state line also drops when it merely restates the badge. */}
      {!(entry.needs && entry.needsReason) &&
        entry.stateLabel.toLowerCase() !== entry.statusLabel.toLowerCase() && (
          <div className="mt-1 text-sm text-foreground/90">{entry.stateLabel}</div>
        )}

      {entry.needs && entry.needsReason && (
        <div
          data-testid="needs-reason"
          className="mt-2 flex items-center gap-1.5 rounded-md border border-warn/30 bg-warn-muted px-2 py-1 text-xs font-medium text-warn"
        >
          <AlertTriangle className="h-3.5 w-3.5 shrink-0" />
          {entry.needsReason} for {age}
        </div>
      )}

      <div className="mt-2 flex flex-wrap items-center gap-x-1.5 gap-y-1">
        <span className="font-mono text-xs text-muted-foreground">{entry.repo}</span>
        {/* On a card the empty state is worth the pixels: this surface is reserved for agents that
            need a decision, and "we don't know what this is working on" is part of that decision. */}
        {entry.task ? <TaskChip task={entry.task} className="max-w-[16rem]" /> : <NoTaskChip />}
        <HarnessMenu target={entry.target} current={entry.harness} harnesses={harnesses} />
        <FleetCounters entry={entry} />
        <Dot />
        <Age ts={entry.updatedAt} now={now} />
        {entry.pr &&
          (href ? (
            <a
              href={href}
              target="_blank"
              rel="noreferrer"
              onClick={(e) => e.stopPropagation()}
              className="inline-flex items-center rounded-md bg-muted px-1.5 py-0.5 text-xs text-primary"
            >
              {prLabel(entry.pr)}
            </a>
          ) : (
            <span className="inline-flex items-center rounded-md bg-muted px-1.5 py-0.5 text-xs text-muted-foreground">
              {prLabel(entry.pr)}
            </span>
          ))}
      </div>

      {/* The actions a stuck agent actually needs from a board. Steering it means reading what it
          said, so that stays behind the card's tap-through rather than duplicating a composer here. */}
      {entry.needs && (
        <div className="mt-2 flex flex-wrap items-center gap-1.5">
          <RelaunchButton target={entry.target} control={entry.control} />
          <CopyButton text={jumpCommand(entry.target)} label={`⧉ ${entry.target}`} />
        </div>
      )}
    </>
  );

  // Base card surface, applied directly (Link variant) or via <Card> (static variant).
  const surface = cn(
    "relative block overflow-hidden rounded-lg border border-border bg-card p-3 pl-4 text-card-foreground shadow-card transition-colors",
    to && "hover:border-border/80 hover:bg-accent/40",
    entry.needs && "border-warn/40",
    isOrchestrator && "border-info/40",
  );

  if (to) {
    return (
      // `overflow-hidden` would clip the harness menu, so the popover-bearing card clips only the
      // tone bar's corners via the rounded surface itself.
      <Link to={to} className={cn(surface, "overflow-visible")}>
        {inner}
      </Link>
    );
  }
  return (
    <Card className={cn(surface, "block overflow-visible")}>
      {inner}
      {!entry.needs && (
        <div className="mt-2">
          <CopyButton text={jumpCommand(entry.target)} label={`⧉ ${entry.target}`} />
        </div>
      )}
    </Card>
  );
}
