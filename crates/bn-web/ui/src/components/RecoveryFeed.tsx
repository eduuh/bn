import { useQuery } from "@tanstack/react-query";
import { recoveryQuery } from "../api/client";
import { fmtClock } from "../lib/format";
import { RowsSkeleton, SectionCard } from "./SectionCard";
import { StateChip } from "./StateChip";

/** The live recovery / lifecycle feed (newest first) across every run. Hydrated from
 * `/api/orchestrate/recovery` and refetched whenever an `orchestrate` SSE frame lands (the run
 * file only rewrites when its recovery log changes), so it appends live without a reload. */
export function RecoveryFeed() {
  const { data, isLoading } = useQuery(recoveryQuery());
  const rows = data?.recovery ?? [];

  // Keep the Fleet glance clean: the recovery log only appears once there are events.
  if (!isLoading && rows.length === 0) return null;

  return (
    <SectionCard title="Recovery feed" count={rows.length}>
      {isLoading ? (
        <RowsSkeleton rows={2} className="h-8" />
      ) : (
        <ul className="flex flex-col gap-2">
          {rows.map((r, i) => (
            <li
              key={`${r.ts}-${r.id}-${i}`}
              className="flex flex-wrap items-baseline gap-x-3 gap-y-1 rounded-sm border border-border bg-background px-3 py-2 text-sm"
            >
              <span className="font-mono text-xs text-muted-foreground">{fmtClock(r.ts)}</span>
              <span className="font-mono text-xs">{r.repo ? `${r.repo}#${r.id}` : r.id}</span>
              <StateChip state={r.state} />
              <span className="text-muted-foreground" aria-hidden="true">
                →
              </span>
              <span className="font-semibold text-primary">{r.action}</span>
              <span className="min-w-0 flex-1 truncate text-muted-foreground">{r.note}</span>
            </li>
          ))}
        </ul>
      )}
    </SectionCard>
  );
}
