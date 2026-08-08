import { useMemo, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Clock } from "lucide-react";

import { queueTimelineQuery } from "../api/client";
import type { TimelineMark } from "../api/queue-types";
import { SectionCard } from "./SectionCard";
import { Badge } from "./ui/badge";
import { Skeleton } from "./ui/skeleton";
import { cn } from "../lib/utils";

/**
 * What is next, with exact times.
 *
 * This survived a cull that removed everything the timeline already showed, and it should not have
 * been in it: the timeline says *that* something is coming and roughly where in the day it sits,
 * which is a different question from *when, in what order, and how soon*. Reading a clock time off a
 * bar is guesswork; that is what a list is for. The two are complements, not duplicates — which is
 * only obvious once the list is gone.
 *
 * Deliberately not Running or Done. Those are single facts the timeline states adequately and the
 * task list carries in full; upcoming is the one that needs precision, because it is the one you act
 * on.
 */

const CLASSES = [
  "all",
  "foreground",
  "autonomous",
  "opportunistic",
  "cron",
  "warming",
  "recovery",
  "maintenance",
] as const;

function clock(at: number): string {
  return new Date(at * 1000).toLocaleTimeString(undefined, {
    hour: "2-digit",
    minute: "2-digit",
  });
}

/** Minutes until `at`, phrased as a wait rather than a timestamp. */
function until(at: number, now: number): string {
  const mins = Math.round((at - now) / 60);
  if (mins <= 0) return "due";
  if (mins < 60) return `in ${mins}m`;
  const hrs = Math.floor(mins / 60);
  const rem = mins % 60;
  return rem === 0 ? `in ${hrs}h` : `in ${hrs}h ${rem}m`;
}

export function QueueUpcoming() {
  const [klass, setKlass] = useState<string>("all");
  const timeline = useQuery(queueTimelineQuery(undefined, klass === "all" ? "" : klass));
  const view = timeline.data;

  const rows = useMemo(() => {
    if (!view) return [];
    const band = (id: string) => view.bands.find((b) => b.id === id)?.marks ?? [];
    // Two corrections the raw bands need for this question. The band carries the *whole* day, so
    // at 02:00 the list would open with firings that already happened — and a section called
    // "still to come" is the one place a past time is simply false. And a minutely cron contributes
    // 1440 marks, which buries every other job under its own repetition.
    const future = [...band("scheduled"), ...band("projected")]
      .filter((m) => m.at >= view.now)
      .sort((a, b) => a.at - b.at);
    const byLabel = new Map<string, { mark: TimelineMark; more: number }>();
    for (const mark of future) {
      const seen = byLabel.get(mark.label);
      if (seen) seen.more += 1;
      else byLabel.set(mark.label, { mark, more: 0 });
    }
    return [...byLabel.values()];
  }, [view]);

  return (
    <SectionCard
      eyebrow="next"
      title="Still to come"
      count={timeline.isLoading ? undefined : String(rows.length)}
    >
      <div className="mb-3 flex flex-wrap gap-1" role="group" aria-label="Timeline class">
        {CLASSES.map((c) => (
          <button
            key={c}
            type="button"
            className={cn(
              "rounded-full border px-2 py-0.5 text-xs transition",
              klass === c
                ? "border-primary bg-primary text-primary-foreground"
                : "border-border bg-secondary text-secondary-foreground hover:bg-secondary/80",
            )}
            aria-pressed={klass === c}
            onClick={() => setKlass(c)}
          >
            {c}
          </button>
        ))}
      </div>
      {timeline.isLoading ? (
        <Skeleton className="h-12 w-full" />
      ) : rows.length === 0 ? (
        <p className="text-sm text-muted-foreground">Nothing else is scheduled today.</p>
      ) : (
        <ul className="space-y-1.5">
          {rows.slice(0, 14).map(({ mark, more }) => (
            <li key={mark.key} className="flex items-baseline gap-2 text-sm">
              <Clock
                className={cn(
                  "size-3 shrink-0",
                  mark.projected ? "text-muted-foreground/60" : "text-muted-foreground",
                )}
              />
              <time
                className="shrink-0 font-mono text-xs tabular-nums text-muted-foreground"
                dateTime={new Date(mark.at * 1000).toISOString()}
              >
                {clock(mark.at)}
              </time>
              <Badge variant="secondary" className="shrink-0">
                {mark.kind}
              </Badge>
              <span className="min-w-0 flex-1 truncate" title={mark.note || mark.label}>
                {mark.label}
              </span>
              {/* A projection drawn like a scheduled time is a lie told every time it is wrong. */}
              {mark.projected ? (
                <span className="shrink-0 text-[11px] text-muted-foreground">est.</span>
              ) : null}
              {more > 0 ? (
                <span className="shrink-0 text-[11px] text-muted-foreground">+{more} today</span>
              ) : null}
              <span className="shrink-0 text-[11px] tabular-nums text-muted-foreground">
                {until(mark.at, view?.now ?? 0)}
              </span>
            </li>
          ))}
        </ul>
      )}
      {rows.length > 14 ? (
        <p className="mt-2 text-[11px] text-muted-foreground">
          Showing the next 14 of {rows.length}.
        </p>
      ) : null}
    </SectionCard>
  );
}
