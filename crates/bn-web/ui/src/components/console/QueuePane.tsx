import { useEffect, useRef } from "react";
import { Search } from "lucide-react";
import { byLane, LANE_EMPTY, LANE_LABEL, type WorkItem } from "../../lib/console";
import { timeAgo } from "../../lib/format";
import { cn } from "../../lib/utils";

/** Left accent-bar colour per tone — the only colour most rows carry, so the list scans vertically
 * by urgency without becoming a rainbow. */
const BAR: Record<WorkItem["tone"], string> = {
  urgent: "bg-urgent",
  warn: "bg-warn",
  ok: "bg-ok",
  info: "bg-info/70",
  muted: "bg-border",
};

/** Tag colour per tone — muted chips, since the bar already carries the signal. */
const TAG: Record<WorkItem["tone"], string> = {
  urgent: "text-urgent",
  warn: "text-warn",
  ok: "text-ok",
  info: "text-info",
  muted: "text-muted-foreground",
};

/**
 * One queue row. Deliberately **two lines and ~52px** — the old command center spent 76–105px of a
 * card per single line of text, so only six of nineteen items were ever on screen. Density is the
 * feature here: the whole queue should be visible without scrolling.
 */
function QueueRow({
  item,
  selected,
  now,
  onSelect,
}: {
  item: WorkItem;
  selected: boolean;
  now: number;
  onSelect: (id: string) => void;
}) {
  const ref = useRef<HTMLButtonElement>(null);

  // Keep the keyboard cursor in view as j/k walks past the fold.
  useEffect(() => {
    if (selected) ref.current?.scrollIntoView({ block: "nearest" });
  }, [selected]);

  return (
    <button
      ref={ref}
      type="button"
      onClick={() => onSelect(item.id)}
      aria-current={selected}
      className={cn(
        "relative flex w-full flex-col gap-0.5 border-l-2 py-2 pl-3 pr-2.5 text-left transition-colors",
        selected
          ? "border-l-primary bg-primary/10"
          : "border-l-transparent hover:bg-accent/50",
      )}
    >
      {/* Tone bar sits inside the row so the selected state can override it with the brand edge. */}
      {!selected && (
        <span
          className={cn("absolute inset-y-1 -left-0.5 w-0.5 rounded-full", BAR[item.tone])}
          aria-hidden="true"
        />
      )}
      <div className="flex items-baseline gap-2">
        <span className="min-w-0 flex-1 truncate text-[0.82rem] font-medium leading-snug text-foreground">
          {item.title}
        </span>
        <span className="shrink-0 text-[0.66rem] tabular-nums text-muted-foreground/60">
          {timeAgo(item.updatedAt, now)}
        </span>
      </div>
      <div className="flex items-center gap-1.5 text-[0.68rem] leading-tight text-muted-foreground">
        <span className={cn("shrink-0 font-medium uppercase tracking-wide", TAG[item.tone])}>
          {item.kind}
        </span>
        {item.where && (
          <span className="min-w-0 truncate font-mono text-muted-foreground/70">{item.where}</span>
        )}
      </div>
    </button>
  );
}

/**
 * The console's left pane: the entire supervision queue in one ranked, grouped, filterable list.
 * Selection is the whole interaction — clicking (or `j`/`k`) moves the focus pane beside it, so you
 * never navigate away from the queue to act on something.
 */
export function QueuePane({
  items,
  selectedId,
  query,
  now,
  onSelect,
  onQuery,
}: {
  items: WorkItem[];
  selectedId?: string;
  query: string;
  now: number;
  onSelect: (id: string) => void;
  onQuery: (q: string) => void;
}) {
  const lanes = byLane(items);

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="flex-none border-b border-border px-3 py-2.5">
        <div className="relative">
          <Search className="pointer-events-none absolute left-2.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground/60" />
          <input
            value={query}
            onChange={(e) => onQuery(e.target.value)}
            placeholder="Filter queue…"
            aria-label="filter the queue"
            className="h-8 w-full rounded-md border border-border bg-background pl-8 pr-2 text-[0.8rem] text-foreground placeholder:text-muted-foreground/60 focus:border-primary/50 focus:outline-none"
          />
        </div>
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto pb-4">
        {lanes.map(({ lane, items: laneItems }) => (
          <section key={lane} className="pt-1">
            <h3 className="sticky top-0 z-10 flex items-center gap-1.5 bg-card/95 px-3 py-1.5 text-[0.64rem] font-semibold uppercase tracking-[0.09em] text-muted-foreground/80 backdrop-blur">
              {LANE_LABEL[lane]}
              {laneItems.length > 0 && (
                <span className="tabular-nums text-muted-foreground/50">{laneItems.length}</span>
              )}
            </h3>
            {laneItems.length === 0 ? (
              <p className="px-3 pb-2 pt-0.5 text-[0.7rem] leading-relaxed text-muted-foreground/50">
                {query ? "No matches in this lane." : LANE_EMPTY[lane]}
              </p>
            ) : (
              laneItems.map((it) => (
                <QueueRow
                  key={it.id}
                  item={it}
                  selected={it.id === selectedId}
                  now={now}
                  onSelect={onSelect}
                />
              ))
            )}
          </section>
        ))}
      </div>
    </div>
  );
}
