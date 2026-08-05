import { Link } from "react-router-dom";
import { AlertTriangle, ChevronRight } from "lucide-react";
import type { FleetEntry } from "../lib/fleet";
import type { HarnessOption } from "../api/types";
import { fmtTokens, timeAgo } from "../lib/format";
import { prHref, prLabel, jumpCommand } from "../lib/board";
import { CopyButton } from "./CopyButton";
import { HarnessSwitcher } from "./HarnessSwitcher";
import { Badge } from "./ui/badge";
import { Card } from "./ui/card";
import { toneVariant } from "./StateChip";
import { cn } from "../lib/utils";

/** The left tone bar colour for a card's state. */
const BAR: Record<string, string> = {
  green: "bg-ok",
  red: "bg-urgent",
  amber: "bg-warn",
  grey: "bg-border",
  neutral: "bg-border",
};

/** A small metadata tag on a card's footer row. */
function Tag({ children, className }: { children: React.ReactNode; className?: string }) {
  return (
    <span
      className={cn(
        "inline-flex items-center rounded-md bg-muted px-1.5 py-0.5 text-xs text-muted-foreground",
        className,
      )}
    >
      {children}
    </span>
  );
}

/**
 * The glanceable Fleet card (#479): status · repo:branch · one-line state · tokens · age, on one
 * tap-sized surface. Taps through to the agent detail when a live session backs it, or to the
 * Reviews inbox when it's paused for plan-review; otherwise it stays a static glance card with a
 * copy-to-jump target. The left border colours by state so the board scans at a glance. A live
 * agent also carries a harness switcher (#501) to relaunch it under another harness in place.
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

  const inner = (
    <>
      <span className={cn("absolute inset-y-0 left-0 w-1", BAR[entry.tone] ?? BAR.neutral)} aria-hidden="true" />
      <div className="flex items-center gap-2">
        <span className="truncate font-mono text-sm font-semibold text-foreground">{entry.repo}</span>
        {isOrchestrator && (
          <Badge variant="info" className="shrink-0 gap-1">
            ◆ Orchestrator
          </Badge>
        )}
        <Badge variant={toneVariant(entry.tone)} className="shrink-0">
          {entry.statusLabel}
        </Badge>
        <span className="flex-1" />
        {to && <ChevronRight className="h-4 w-4 shrink-0 text-muted-foreground" />}
      </div>

      <div className="mt-1 truncate font-mono text-xs text-muted-foreground">{entry.branch}</div>
      <div className="mt-1 text-sm text-foreground/90">{entry.stateLabel}</div>

      {entry.needs && entry.needsReason && (
        <div
          data-testid="needs-reason"
          className="mt-2 flex items-center gap-1.5 rounded-md border border-warn/30 bg-warn-muted px-2 py-1 text-xs font-medium text-warn"
        >
          <AlertTriangle className="h-3.5 w-3.5 shrink-0" />
          {entry.needsReason}
        </div>
      )}

      <div className="mt-2 flex flex-wrap items-center gap-1.5">
        {entry.harness && <Tag>{entry.harness}</Tag>}
        {entry.outputTokens ? <Tag>{fmtTokens(entry.outputTokens)} tok</Tag> : null}
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
            <Tag>{prLabel(entry.pr)}</Tag>
          ))}
        <Tag className="text-muted-foreground/70">{timeAgo(entry.updatedAt, now)}</Tag>
      </div>

      {entry.harness && (
        <div className="mt-2">
          <HarnessSwitcher target={entry.target} current={entry.harness} harnesses={harnesses} />
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
      <Link to={to} className={surface}>
        {inner}
      </Link>
    );
  }
  return (
    <Card className={cn(surface, "block")}>
      {inner}
      <div className="mt-2">
        <CopyButton text={jumpCommand(entry.target)} label={`⧉ ${entry.target}`} />
      </div>
    </Card>
  );
}
