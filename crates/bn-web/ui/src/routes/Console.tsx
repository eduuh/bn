import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useSearchParams } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { ChevronLeft } from "lucide-react";
import { actionsQuery, fleetQuery, orchestrateQuery } from "../api/client";
import { fleetEntries } from "../lib/fleet";
import { buildQueue, filterQueue, queueCounts } from "../lib/console";
import { useNow } from "../hooks/useNow";
import { useMediaQuery } from "../hooks/useMediaQuery";
import { QueuePane } from "../components/console/QueuePane";
import { FocusPane } from "../components/console/FocusPane";
import { ActPane } from "../components/console/ActPane";
import { ToastStack } from "../components/ToastStack";
import { cn } from "../lib/utils";

/**
 * `/` — the **console**. One screen for the whole supervision loop.
 *
 * The dashboard used to spread a single job across seventeen routes: you triaged on the command
 * center, watched on the fleet, read the plan on the agent page, and decided in a modal — losing
 * your place at every step. But the operator's loop is one cycle (signal → dispatch → agent asks →
 * you decide → land), and the web's only real job is to shorten *your* turn in it.
 *
 * So: three columns that never navigate.
 *
 * - **Queue** — every open loop, ranked and grouped by what you'd *do* about it, dense enough that
 *   the whole list is on screen at once.
 * - **Focus** — the selected item fully resolved: the session activity, the branch note, or the plan
 *   rendered as real markdown.
 * - **Act** — only the decisions that item actually admits.
 *
 * Selection lives in `?sel=`, so a view is linkable and survives a reload. `j`/`k` walk the queue
 * and `Escape` clears the selection; typing in any field is left alone.
 */
export function Console() {
  const now = useNow();
  const orch = useQuery(orchestrateQuery());
  const fleet = useQuery(fleetQuery());
  const actions = useQuery(actionsQuery());

  const [params, setParams] = useSearchParams();
  const selectedId = params.get("sel") ?? undefined;
  const [query, setQuery] = useState("");
  // `lg` is where the queue and the focus pane can coexist; below it the console is list-or-detail.
  const wide = useMediaQuery("(min-width: 1024px)");

  const all = useMemo(
    () => buildQueue(fleetEntries(orch.data, fleet.data), actions.data?.actions ?? []),
    [orch.data, fleet.data, actions.data],
  );
  const items = useMemo(() => filterQueue(all, query), [all, query]);
  const counts = queueCounts(all);

  const selected = all.find((i) => i.id === selectedId);

  // Stable across renders so the effects below can depend on it honestly: `setParams` is stable, so
  // this identity never changes and neither effect re-runs on account of it.
  const select = useCallback(
    (id?: string) => {
      setParams(
        (prev) => {
          const next = new URLSearchParams(prev);
          if (id) next.set("sel", id);
          else next.delete("sel");
          return next;
        },
        { replace: true },
      );
    },
    [setParams],
  );

  // Land on the top of the queue rather than an empty pane. The list is already ranked so that the
  // first row is the most-blocked thing, which means the answer to "what do I do next?" is on
  // screen before you touch anything. Runs once — after that, selection is yours (Escape clears it
  // and it stays cleared).
  // Waits for all three feeds to settle first: actions resolve well before the fleet does, so
  // selecting on the first non-empty queue would reliably land on a backlog row while the genuinely
  // blocked agent was still loading — exactly the item you didn't need to see.
  // Only on a wide screen, where the focus pane sits *beside* the queue. On a phone, selecting
  // replaces the queue, so auto-selecting would drop you into a detail view you never asked for.
  const autoSelected = useRef(false);
  const ready = !orch.isLoading && !fleet.isLoading && !actions.isLoading;
  useEffect(() => {
    if (autoSelected.current || !wide || !ready || selectedId || all.length === 0) return;
    autoSelected.current = true;
    select(all[0].id);
  }, [wide, ready, all, selectedId, select]);

  // j / k walk the *filtered* queue; Escape clears. Skipped while typing so the filter box and the
  // steer composer keep their keys.
  //
  // `selectedId` is intentionally read from `window.location.search` rather than from the closure:
  // React Router v6 wraps navigation updates in `startTransition`, so the React re-render (and the
  // effect re-registration with the updated `selectedId`) is deferred as a non-urgent transition.
  // The URL itself is updated synchronously by `history.replaceState`, so reading from
  // `window.location` is always current even when the closure value lags.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const el = e.target as HTMLElement | null;
      const typing =
        el &&
        (el.tagName === "INPUT" ||
          el.tagName === "TEXTAREA" ||
          el.isContentEditable ||
          el.getAttribute("role") === "textbox");
      if (typing || e.metaKey || e.ctrlKey || e.altKey) return;

      if (e.key === "j" || e.key === "k") {
        if (items.length === 0) return;
        e.preventDefault();
        const currentSel = new URLSearchParams(window.location.search).get("sel") ?? undefined;
        const at = items.findIndex((i) => i.id === currentSel);
        const step = e.key === "j" ? 1 : -1;
        const next = at === -1 ? (step === 1 ? 0 : items.length - 1) : at + step;
        const clamped = Math.max(0, Math.min(items.length - 1, next));
        select(items[clamped].id);
      } else if (e.key === "Escape") {
        select(undefined);
      }
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [items, select]);

  const loading = orch.isLoading && fleet.isLoading && actions.isLoading;

  return (
    <div className="flex h-full min-h-0 flex-col">
      {/* Three columns on a desktop second screen. Below 1181px the act pane folds *under* the
          focus pane as a horizontal strip (same element, re-flowed — see `ActPane`) rather than
          disappearing, and on a phone the queue becomes a full-width list. */}
      <div
        className={cn(
          "grid min-h-0 flex-1 grid-cols-1",
          "lg:grid-cols-[19.5rem_minmax(0,1fr)] lg:grid-rows-[minmax(0,1fr)_auto]",
          "wide:grid-cols-[19.5rem_minmax(0,1fr)_18.5rem] wide:grid-rows-1",
        )}
      >
        <aside
          aria-label="work queue"
          className={cn(
            "min-h-0 flex-col border-border bg-card lg:flex lg:border-r",
            "lg:col-start-1 lg:row-start-1 lg:row-span-2 wide:row-span-1",
            // Below lg the queue and the focus pane swap rather than stack — a stacked pair gives
            // each about a third of a phone screen, which is unusable for both.
            selected ? "hidden" : "flex",
          )}
        >
          <div className="flex-none px-3.5 pb-2 pt-3.5">
            {/* The prototype's `.queue-head`: the title and the state of the queue on one baseline,
                counts pushed to the right edge where they read as a status line rather than a
                subtitle. */}
            <div className="flex items-baseline gap-2">
              {/* `shrink-0`: the counts wrap to a second line on a busy queue, and without it the
                  flex row squeezed the title until it broke mid-word ("Queu / e"). */}
              <h1 className="shrink-0 font-display text-lg font-semibold tracking-tight text-foreground">
                Queue
              </h1>
              <span className="ml-auto text-right font-mono text-[11px] tabular-nums text-muted-foreground/70">
                {counts.blocked > 0 ? (
                  <span className="font-semibold text-urgent">{counts.blocked} blocked</span>
                ) : (
                  <span className="text-ok">clear</span>
                )}
                <span className="text-muted-foreground/50"> · </span>
                {counts.running} running
                <span className="text-muted-foreground/50"> · </span>
                {counts.backlog} queued
              </span>
            </div>
            {/* No "Start" here. The queue's job is the work that already exists; the rail's clay
                "New task" and ⌘K are the ways in, and a second start button sitting on top of the
                list only competed with them. */}
          </div>
          {loading ? (
            <p className="px-3 py-4 text-xs text-muted-foreground">Loading the queue…</p>
          ) : (
            <QueuePane
              items={items}
              selectedId={selectedId}
              query={query}
              now={now}
              onSelect={select}
              onQuery={setQuery}
            />
          )}
        </aside>

        <section
          aria-label="focus"
          className={cn(
            "min-h-0 flex-col overflow-hidden lg:flex",
            "lg:col-start-2 lg:row-start-1",
            selected ? "flex" : "hidden lg:flex",
          )}
        >
          {/* The way back to the queue on a phone; on a wide screen the queue never left. */}
          {selected && (
            <button
              type="button"
              onClick={() => select(undefined)}
              className="flex flex-none items-center gap-1.5 border-b border-border px-4 py-2 text-left text-[0.72rem] font-medium text-muted-foreground hover:text-foreground lg:hidden"
            >
              <ChevronLeft className="h-4 w-4" /> Queue
            </button>
          )}
          <FocusPane item={selected} blocked={counts.blocked} now={now} />
        </section>

        {/* One instance, three placements: hidden on a phone until something is selected, a
            horizontal strip under Focus at tablet width, the third column at `wide`. */}
        <aside
          aria-label="actions"
          className={cn(
            "min-h-0 overflow-y-auto border-border bg-card",
            "lg:col-start-2 lg:row-start-2 lg:block lg:max-h-[38dvh] lg:border-t",
            "wide:col-start-3 wide:row-start-1 wide:max-h-none wide:border-l wide:border-t-0",
            selected ? "block border-t" : "hidden lg:block",
          )}
        >
          <ActPane item={selected} />
        </aside>
      </div>

      <ToastStack />
    </div>
  );
}
