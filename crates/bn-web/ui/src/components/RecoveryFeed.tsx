import { useQuery } from "@tanstack/react-query";
import { recoveryQuery } from "../api/client";
import { fmtClock } from "../lib/format";
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
    <section className="section recovery">
      <div className="section-head">
        <h3>Recovery feed</h3>
        <span className="count">{rows.length}</span>
      </div>
      {isLoading && <div className="empty">Loading…</div>}
      <ul className="recovery-list">
        {rows.map((r, i) => (
          <li key={`${r.ts}-${r.id}-${i}`} className="recovery-row">
            <span className="ts mono">{fmtClock(r.ts)}</span>
            <span className="rid mono">{r.repo ? `${r.repo}#${r.id}` : r.id}</span>
            <StateChip state={r.state} />
            <span className="arrow">→</span>
            <span className="action">{r.action}</span>
            <span className="note">{r.note}</span>
          </li>
        ))}
      </ul>
    </section>
  );
}
