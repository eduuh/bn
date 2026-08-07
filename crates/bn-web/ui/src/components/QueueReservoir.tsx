import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Link } from "react-router-dom";

import { reservoirQuery } from "../api/client";
import type { QueueReservoir as Reservoir } from "../api/queue-types";
import { SectionCard } from "./SectionCard";
import { StatGrid, StatTile } from "./StatTile";
import { Badge } from "./ui/badge";
import { Input } from "./ui/input";
import { Skeleton } from "./ui/skeleton";

/**
 * The reservoir: work that exists but has not reached the backlog yet.
 *
 * This section is why the page was worth changing. `/queue` read one store — `backlog.jsonl` — and
 * since #1001 retired keep-warm's backlog arm nothing but the cron producer writes to it. Real work
 * is minted as a *task* and only becomes a backlog entry once it has passed the SMART gate, so a
 * machine with thousands of tasks waiting rendered as "0 pending" above a list of cron ticks. The
 * page was not lying about the backlog; it was answering a narrower question than its name
 * promises, and from the outside those two are indistinguishable.
 *
 * The states are ordered by distance from executable rather than by size, because that ordering is
 * the diagnosis: a reservoir that is all `needs scoping` waits on the improve agent, one that is
 * all `ready` waits on capacity, and one that is all `needs you` waits on a person. Three unrelated
 * problems that a single "waiting" number cannot tell apart.
 */

/** The gate's verdicts, in pipeline order. `unjudged` is the server's name for "no `smart:` yet" —
 *  where every freshly pulled task starts, and a first-class state rather than a missing value. */
const STATES = [
  {
    key: "unjudged",
    field: "needs_scoping",
    label: "needs scoping",
    tone: "amber",
    hint: "No SMART verdict yet — the gate sends these to the improve agent before anything may execute them.",
  },
  {
    key: "ok",
    field: "ready",
    label: "ready",
    tone: "green",
    hint: "Passed the gate. Eligible to dispatch as soon as a lane has room.",
  },
  {
    key: "fail",
    field: "failed",
    label: "improving",
    tone: "blue",
    hint: "Refused by the gate; the improve agent is still retrying.",
  },
  {
    key: "stuck",
    field: "stuck",
    label: "needs you",
    tone: "red",
    hint: "Out of improve attempts. These wait on a person, and nothing else will move them.",
  },
] as const;

/** How many rows to draw before a list stops being a list and becomes a wall. */
const SHOWN = 25;

export function QueueReservoir({ reservoir }: { reservoir?: Reservoir | null }) {
  const [state, setState] = useState<string>("unjudged");
  const [repo, setRepo] = useState("");
  const list = useQuery(reservoirQuery({ smart: state, repo: repo.trim() }));

  // An unreadable task store is not an empty one. `queue_data` sends null precisely so this can be
  // said out loud instead of rendering as a calm row of zeroes.
  if (reservoir === null) {
    return (
      <SectionCard eyebrow="reservoir" title="Waiting to be scoped">
        <p className="text-sm text-destructive">
          The task store could not be read, so what is waiting is unknown. This is not an empty
          queue.
        </p>
      </SectionCard>
    );
  }

  const rows = list.data?.tasks ?? [];
  const total = list.data?.count ?? 0;
  const current = STATES.find((s) => s.key === state);

  return (
    <SectionCard
      eyebrow="reservoir"
      title="Waiting to be scoped"
      count={reservoir ? `${reservoir.total} open` : undefined}
    >
      {!reservoir ? (
        <Skeleton className="h-16 w-full" />
      ) : (
        <StatGrid cols={4} className="mb-3 overflow-hidden rounded-md border border-border">
          {STATES.map((s) => (
            <StatTile
              key={s.key}
              label={s.label}
              value={String(reservoir[s.field])}
              tone={s.tone}
              title={s.hint}
              // The tiles are the filter. A count you can see but not open is a number that sends
              // you somewhere else to find the thing it counts.
              role="button"
              tabIndex={0}
              aria-pressed={state === s.key}
              onClick={() => setState(s.key)}
              onKeyDown={(e) => {
                if (e.key === "Enter" || e.key === " ") {
                  e.preventDefault();
                  setState(s.key);
                }
              }}
              className={
                state === s.key
                  ? "cursor-pointer ring-1 ring-inset ring-primary"
                  : "cursor-pointer hover:bg-accent/40"
              }
            />
          ))}
        </StatGrid>
      )}

      <div className="mb-2 flex flex-wrap items-center gap-2">
        <Input
          value={repo}
          onChange={(e) => setRepo(e.target.value)}
          placeholder="filter by repo"
          aria-label="Filter reservoir by repo"
          className="h-8 w-40"
        />
        {current && <span className="text-xs text-muted-foreground">{current.hint}</span>}
      </div>

      {list.isLoading ? (
        <Skeleton className="h-24 w-full" />
      ) : rows.length === 0 ? (
        <p className="text-sm text-muted-foreground">
          Nothing {current?.label ?? state}
          {repo.trim() ? ` in ${repo.trim()}` : ""}.
        </p>
      ) : (
        <ul className="divide-y divide-border">
          {rows.slice(0, SHOWN).map((t) => (
            <li key={t.id} className="flex items-center gap-2 py-1 text-sm">
              <Link
                to={`/tasks/${encodeURIComponent(t.id)}`}
                className="shrink-0 font-mono text-xs text-muted-foreground hover:text-foreground"
              >
                {t.id}
              </Link>
              <span className="min-w-0 flex-1 truncate" title={t.summary}>
                {t.summary}
              </span>
              {/* Provenance worth seeing at a glance: an issue-backed task came from the reservoir
                  refill, a bare one was typed by someone. */}
              {t.src?.startsWith("http") ? (
                <Badge variant="secondary" className="shrink-0">
                  issue
                </Badge>
              ) : null}
              <span className="shrink-0 font-mono text-[11px] text-muted-foreground">
                {t.repo ?? "—"}
              </span>
            </li>
          ))}
        </ul>
      )}
      {total > Math.min(rows.length, SHOWN) ? (
        <p className="mt-2 text-[11px] text-muted-foreground">
          Showing {Math.min(rows.length, SHOWN)} of {total}.
        </p>
      ) : null}
    </SectionCard>
  );
}
