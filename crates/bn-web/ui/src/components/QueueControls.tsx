import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { ChevronDown, ChevronUp, X } from "lucide-react";

import { queueEntriesQuery } from "../api/client";
import { queueCancel, queueClass, queuePriority, queueSnooze } from "../api/writes";
import { useAppMutation } from "../hooks/useAppMutation";
import { affects } from "../lib/invalidation";
import type { QueueEntry } from "../api/queue-types";
import { ConfirmButton } from "./ConfirmButton";
import { SectionCard } from "./SectionCard";
import { Badge } from "./ui/badge";
import { Button } from "./ui/button";
import { Input } from "./ui/input";

/**
 * The admitted backlog's write surface: take pending work out or change its order.
 *
 * Admission belongs to Today; once work is here, Queue owns the operational decisions: this should
 * run sooner, later, or not at all.
 */

/** How far one nudge moves an entry. Large enough to actually cross a neighbour, since queued work
 * clusters at a handful of round numbers rather than spreading evenly. */
const STEP = 10;

/** The states an entry can be in, in lifecycle order. `all` first because it is the default. */
const STATUSES = ["all", "pending", "leased", "done", "failed", "cancelled"] as const;

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

/** Status → badge tone. Cancelled reads as neither success nor failure, because it is neither. */
const STATUS_TONE: Record<string, "secondary" | "destructive"> = {
  failed: "destructive",
};

/** One entry, with whatever you can do to it — which depends on what state it is in. */
function EntryRow({ entry }: { entry: QueueEntry }) {
  // No optimistic patch on either: the entry lists are keyed by the filter in play, which this row
  // does not know, so guessing would mean threading the parent's filter state down just to patch
  // the right cache. The reconcile covers every filter at once instead.
  const bump = useAppMutation<unknown, number>({
    mutationFn: (priority) => queuePriority(entry.repo, entry.id, priority),
    invalidates: affects.queue,
    toast: { error: "Could not reprioritize" },
  });
  const cancel = useAppMutation({
    mutationFn: () => queueCancel(entry.repo, entry.id),
    invalidates: affects.queue,
    toast: { success: `Cancelled ${entry.id}`, error: "Could not cancel" },
  });
  const snooze = useAppMutation({
    mutationFn: () => queueSnooze(entry.repo, entry.id, "30m"),
    invalidates: affects.queue,
    toast: { success: `Snoozed ${entry.id}`, error: "Could not snooze" },
  });
  const markAway = useAppMutation({
    mutationFn: () => queueClass(entry.repo, entry.id, "opportunistic"),
    invalidates: affects.queue,
    toast: { success: `${entry.id} marked opportunistic`, error: "Could not change class" },
  });
  const markWatched = useAppMutation({
    mutationFn: () => queueClass(entry.repo, entry.id, "foreground"),
    invalidates: affects.queue,
    toast: { success: `${entry.id} marked foreground`, error: "Could not change class" },
  });
  const busy =
    bump.isPending ||
    cancel.isPending ||
    snooze.isPending ||
    markAway.isPending ||
    markWatched.isPending;
  // Only pending work can be moved or taken back out. A leased entry has an executor running, and
  // a terminal one is over — offering controls that the CLI would refuse would be a button that
  // lies, so the row simply shows what state it reached instead.
  const actionable = entry.status === "pending";

  return (
    <li className="flex items-center gap-2 py-1 text-sm">
      <span className="w-10 shrink-0 text-right font-mono text-xs tabular-nums text-muted-foreground">
        {entry.priority}
      </span>
      <Badge variant="secondary" className="shrink-0">
        {entry.kind}
      </Badge>
      <span className="min-w-0 flex-1 truncate" title={entry.item}>
        {entry.item}
      </span>
      {!actionable && (
        <Badge variant={STATUS_TONE[entry.status] ?? "secondary"} className="shrink-0">
          {entry.status}
        </Badge>
      )}
      <span className="shrink-0 font-mono text-[11px] text-muted-foreground">{entry.repo}</span>
      {!actionable ? null : (
        <span className="flex shrink-0 items-center gap-1">
          <Button
            size="icon"
            variant="ghost"
            disabled={busy}
            aria-label={`Raise ${entry.id}`}
            title="Run sooner"
            onClick={() => bump.mutate(entry.priority + STEP)}
          >
            <ChevronUp />
          </Button>
          <Button
            size="icon"
            variant="ghost"
            disabled={busy}
            aria-label={`Lower ${entry.id}`}
            title="Run later"
            onClick={() => bump.mutate(entry.priority - STEP)}
          >
            <ChevronDown />
          </Button>
          <Button size="sm" variant="ghost" disabled={busy} onClick={() => snooze.mutate()}>
            30m
          </Button>
          <Button size="sm" variant="ghost" disabled={busy} onClick={() => markWatched.mutate()}>
            watch
          </Button>
          <Button size="sm" variant="ghost" disabled={busy} onClick={() => markAway.mutate()}>
            away
          </Button>
          {/* Confirmed, because cancelling is the one control here that loses something: the entry
            is terminal afterwards and the producer that queued it may not queue it again. */}
          <ConfirmButton
            size="icon"
            variant="ghost"
            disabled={busy}
            title={`Cancel ${entry.id}?`}
            description={
              <>
                Takes <code className="font-mono text-foreground">{entry.item}</code> out of the
                queue. It will not run unless something queues it again.
              </>
            }
            confirmLabel="Cancel it"
            onConfirm={() => cancel.mutate()}
          >
            <X />
          </ConfirmButton>
        </span>
      )}
    </li>
  );
}

export function QueueControls() {
  const [status, setStatus] = useState<string>("pending");
  const [klass, setKlass] = useState<string>("all");
  const [repo, setRepo] = useState<string>("");
  const list = useQuery(
    queueEntriesQuery({
      status: status === "all" ? "" : status,
      repo,
      kind: "",
      class: klass === "all" ? "" : klass,
    }),
  );

  const rows = list.data?.entries ?? [];
  const counts = list.data?.counts ?? {};
  const total = list.data?.total ?? 0;
  const unreadable = list.data?.unreadable ?? 0;
  // Pending reads as a plan, so it sorts by the order the dispatcher will take it in. Everything
  // else is history, and history reads newest-first — which is how the server already sends it.
  const shown = status === "pending" ? [...rows].sort((a, b) => b.priority - a.priority) : rows;

  return (
    <SectionCard eyebrow="dispatched" title="Backlog entries" count={String(total)}>
      <div className="mb-3 flex flex-wrap items-center gap-2">
        <div className="flex flex-wrap gap-1" role="group" aria-label="Status">
          {STATUSES.map((s) => {
            const n = s === "all" ? undefined : counts[s];
            // A state with nothing in it is still offered — its absence is an answer, and a chip
            // that vanishes when a filter empties makes the set look like it changed.
            return (
              <Button
                key={s}
                size="sm"
                variant={status === s ? "default" : "secondary"}
                aria-pressed={status === s}
                onClick={() => setStatus(s)}
              >
                {s}
                {n !== undefined ? <span className="ml-1 opacity-70">{n}</span> : null}
              </Button>
            );
          })}
        </div>
        <div className="flex flex-wrap gap-1" role="group" aria-label="Class">
          {CLASSES.map((c) => (
            <Button
              key={c}
              size="sm"
              variant={klass === c ? "default" : "secondary"}
              aria-pressed={klass === c}
              onClick={() => setKlass(c)}
            >
              {c}
            </Button>
          ))}
        </div>
        <Input
          value={repo}
          onChange={(e) => setRepo(e.target.value)}
          placeholder="filter by repo"
          aria-label="Filter by repo"
          className="h-8 w-40"
        />
      </div>

      {unreadable > 0 ? (
        <p className="mb-2 text-sm text-destructive">
          {unreadable} unreadable line(s) in the backlog — that many entries are lost.
        </p>
      ) : null}

      {list.isLoading ? (
        <p className="text-sm text-muted-foreground">loading…</p>
      ) : shown.length === 0 ? (
        <p className="text-sm text-muted-foreground">
          {status === "all" ? "The backlog is empty." : `Nothing ${status}.`}
        </p>
      ) : (
        <ul className="divide-y divide-border">
          {shown.map((e) => (
            <EntryRow key={`${e.repo}/${e.id}`} entry={e} />
          ))}
        </ul>
      )}
      {total > shown.length ? (
        <p className="mt-2 text-[11px] text-muted-foreground">
          Showing {shown.length} of {total}.
        </p>
      ) : null}
    </SectionCard>
  );
}
