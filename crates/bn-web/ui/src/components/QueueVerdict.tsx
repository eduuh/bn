import { useMemo, useState } from "react";

import type { QueueDeferredRow, QueueView } from "../api/queue-types";
import { Empty, LoadError, RowsSkeleton, SectionCard } from "./SectionCard";
import { Badge, type BadgeProps } from "./ui/badge";
import { timeAgo } from "../lib/format";
import { cn } from "../lib/utils";

/**
 * The answer to "why is nothing running", stated before anything else on the page.
 *
 * The queue page already carried every fact needed to answer that question and still could not
 * answer it: the hold reasons sat nine cards down as a flat list of kebab-case labels, and
 * `machine.presence` — the gate that actually decides whether opportunistic work may start — was
 * fetched on every poll and never rendered. A page that shows eleven rows reading `awaiting-away`
 * without ever saying *what the machine is waiting for* has reported the symptom and withheld the
 * cause.
 *
 * So this card leads with a verdict sentence, then names the gates that produced it, then groups
 * the held entries by reason instead of repeating one label eleven times.
 *
 * The class chip on each held row is the non-obvious part, and the reason the entries are joined
 * back to `data.entries` at all: an entry's *derived* class is what routes it into the
 * opportunistic lane, and a derivation can be wrong. Showing `opportunistic · warming` next to a
 * repo-setup agent is what makes a misclassification legible as a misclassification rather than as
 * a machine that is merely busy.
 */

/** What each dispatcher hold reason means, in the words someone reading the page would use.
 *  Keyed by `DeferReason::label()` in `bn_core::queue::tick`. */
const REASON_HELP: Record<string, string> = {
  "awaiting-away":
    "opportunistic work only starts once the machine is idle or inside an away window",
  capacity: "the lane is already at its ceiling",
  "kind-cap": "this kind hit its per-tick cap",
  conflict: "another entry holds the same worktree",
  "cant-merge": "the branch does not merge cleanly",
  "no-capable-node": "no ready node advertises what it needs",
  "not-my-kind": "a different dispatcher owns this kind",
  "express-budget": "this pass already spent its inline budget",
  expired: "its not-after deadline passed",
  snoozed: "snoozed until later",
};

const REASON_TONE: Record<string, BadgeProps["variant"]> = {
  "awaiting-away": "info",
  capacity: "warn",
  "kind-cap": "warn",
  conflict: "warn",
  "cant-merge": "urgent",
  "no-capable-node": "urgent",
  expired: "urgent",
  snoozed: "muted",
};

/** One gate the tick consults, and whether it is currently open. `blocking` is what gets the
 *  attention tone — an open gate is context, a closed one is the answer. */
interface Gate {
  label: string;
  state: string;
  detail?: string;
  blocking: boolean;
}

function GateRow({ gate }: { gate: Gate }) {
  return (
    <div className="flex flex-wrap items-baseline gap-x-2 gap-y-0.5 py-1.5">
      <span className="w-24 shrink-0 font-mono text-[10px] uppercase tracking-[0.12em] text-muted-foreground">
        {gate.label}
      </span>
      <span className={cn("text-sm font-medium", gate.blocking ? "text-warn" : "text-ok")}>
        {gate.state}
      </span>
      {gate.detail ? (
        <span className="text-[11px] text-muted-foreground">— {gate.detail}</span>
      ) : null}
    </div>
  );
}

/** The gates in the order the tick applies them, most global first. */
function readGates(data: QueueView): Gate[] {
  const gates: Gate[] = [];
  const machine = data.machine;

  gates.push({
    label: "dispatcher",
    state: machine?.dispatcher_alive ? "online" : "offline",
    detail: machine?.dispatcher_alive
      ? undefined
      : "no tick is running, so nothing will start on its own",
    blocking: !machine?.dispatcher_alive,
  });

  gates.push({
    label: "accepting",
    state: data.accepting ? "yes" : "paused",
    detail: data.accepting ? undefined : "resume with `bn queue pause --clear`",
    blocking: !data.accepting,
  });

  // The gate the old page never showed. `idle` is machine-wide quiet, not "the human stepped
  // away" — anything that writes an event resets it, agents included.
  const presence = machine?.presence;
  if (presence) {
    const open = presence.idle || presence.away_window;
    gates.push({
      label: "presence",
      state: presence.idle ? "idle" : "active",
      detail: open
        ? presence.away_window
          ? "inside an away window — opportunistic work may start"
          : "quiet long enough — opportunistic work may start"
        : `opportunistic work is held until ${presence.opportunistic_idle_mins}m pass with no recorded activity`,
      blocking: !open,
    });
  }

  const lanes = machine?.lanes ?? [];
  const full = lanes.filter((lane) => lane.cap > 0 && lane.used >= lane.cap);
  if (lanes.length > 0) {
    gates.push({
      label: "lanes",
      state: full.length === 0 ? "room free" : `${full.map((l) => l.lane).join(", ")} full`,
      detail: lanes.map((lane) => `${lane.lane} ${lane.used}/${lane.cap}`).join(" · "),
      blocking: full.length > 0,
    });
  }

  const slots = data.nodes.reduce((sum, node) => sum + node.slots, 0);
  const free = data.nodes.reduce((sum, node) => sum + node.free, 0);
  gates.push({
    label: "slots",
    state: data.nodes.length === 0 ? "no nodes" : `${free}/${slots} free`,
    detail:
      data.nodes.length === 0
        ? "nothing is registered to run work"
        : free === 0
          ? "every worker slot is occupied"
          : undefined,
    blocking: data.nodes.length === 0 || free === 0,
  });

  return gates;
}

/** The one-sentence verdict, plus the chip that carries it at a glance. */
function readVerdict(
  data: QueueView,
  gates: Gate[],
): { headline: string; sub: string; state: string; tone: BadgeProps["variant"] } {
  const started = data.decision?.started ?? 0;
  const held = data.decision?.deferred.length ?? 0;
  const blocking = gates.filter((gate) => gate.blocking);

  if (!data.machine?.dispatcher_alive) {
    return {
      headline: "The dispatcher is offline",
      sub: "Queued work will not start until it is running again.",
      state: "offline",
      tone: "urgent",
    };
  }
  if (data.pending === 0 && started === 0) {
    return {
      headline: "Nothing is queued",
      sub: "The dispatcher is running and idle because there is no admitted work to start.",
      state: "empty",
      tone: "muted",
    };
  }
  if (started > 0) {
    return {
      headline: `Started ${started} this pass`,
      sub:
        held > 0
          ? `${held} more stayed put — the reasons are below.`
          : "Everything admitted was dispatched.",
      state: "dispatching",
      tone: "ok",
    };
  }
  return {
    headline: `Nothing started · ${held || data.pending} held`,
    sub: blocking.length
      ? `Blocked on ${blocking.map((gate) => gate.label).join(" and ")}.`
      : "The dispatcher considered the queue and started none of it.",
    state: blocking.length ? "stalled" : "holding",
    tone: "warn",
  };
}

/** Held entries collapsed to one row per reason — eleven identical labels is not eleven facts. */
function groupByReason(
  rows: QueueDeferredRow[],
): Array<{ reason: string; rows: QueueDeferredRow[] }> {
  const byReason = new Map<string, QueueDeferredRow[]>();
  for (const row of rows) {
    const bucket = byReason.get(row.reason);
    if (bucket) bucket.push(row);
    else byReason.set(row.reason, [row]);
  }
  return [...byReason.entries()]
    .map(([reason, rows]) => ({ reason, rows }))
    .sort((a, b) => b.rows.length - a.rows.length);
}

function HeldGroup({
  reason,
  rows,
  data,
  now,
}: {
  reason: string;
  rows: QueueDeferredRow[];
  data: QueueView;
  now: number;
}) {
  const [open, setOpen] = useState(false);
  // The derived class lives on the entry, not on the decision row, so the two are joined here.
  const classOf = (row: QueueDeferredRow) =>
    data.entries.find((view) => view.entry.id === row.id && view.entry.repo === row.repo)?.class;

  return (
    <li className="py-2">
      <button
        type="button"
        onClick={() => setOpen((prev) => !prev)}
        aria-expanded={open}
        className="flex w-full flex-wrap items-baseline gap-x-2 gap-y-1 text-left"
      >
        <Badge variant={REASON_TONE[reason] ?? "muted"}>{reason}</Badge>
        <span className="font-mono text-sm tabular-nums">{rows.length}</span>
        <span className="text-[11px] text-muted-foreground">
          {REASON_HELP[reason] ?? "held by the dispatcher"}
        </span>
        <span className="ml-auto font-mono text-[11px] text-muted-foreground">
          {open ? "hide" : "show"}
        </span>
      </button>
      {open ? (
        <ul className="mt-1.5 space-y-1 border-l border-border pl-3">
          {rows.map((row) => {
            const klass = classOf(row);
            return (
              <li
                key={`${row.repo}/${row.id}`}
                className="flex flex-wrap items-baseline gap-x-2 gap-y-0.5 text-xs"
              >
                <span className="font-mono text-muted-foreground">
                  {row.repo}/{row.id}
                </span>
                <span className="text-muted-foreground">{row.kind}</span>
                {klass ? (
                  <Badge variant={klass.opportunistic ? "info" : "muted"}>
                    {klass.mode} · {klass.subtype}
                  </Badge>
                ) : null}
                <span className="ml-auto font-mono text-muted-foreground">
                  waiting {timeAgo(row.waiting_since, now)}
                </span>
              </li>
            );
          })}
        </ul>
      ) : null}
    </li>
  );
}

export function QueueVerdict({
  data,
  loading,
  error,
}: {
  data?: QueueView;
  loading: boolean;
  error: boolean;
}) {
  const gates = useMemo(() => (data ? readGates(data) : []), [data]);
  const verdict = useMemo(() => (data ? readVerdict(data, gates) : null), [data, gates]);

  if (error) {
    return (
      <SectionCard eyebrow="verdict" title="Why nothing is running">
        <LoadError>Could not load the dispatcher decision.</LoadError>
      </SectionCard>
    );
  }
  if (loading || !data || !verdict) {
    return (
      <SectionCard eyebrow="verdict" title="Why nothing is running">
        <RowsSkeleton rows={3} />
      </SectionCard>
    );
  }

  const held = data.decision?.deferred ?? [];
  const groups = groupByReason(held);

  return (
    <SectionCard
      eyebrow="verdict"
      title={verdict.headline}
      count={
        data.decision
          ? `decided ${timeAgo(data.decision.at, data.now)} ago`
          : "no decision recorded"
      }
      action={<Badge variant={verdict.tone}>{verdict.state}</Badge>}
    >
      <p className="text-sm text-muted-foreground">{verdict.sub}</p>

      <div className="mt-2.5 divide-y divide-border border-y border-border">
        {gates.map((gate) => (
          <GateRow key={gate.label} gate={gate} />
        ))}
      </div>

      <div className="mt-3">
        <p className="mb-1 font-mono text-[10px] uppercase tracking-[0.12em] text-muted-foreground">
          held work
        </p>
        {groups.length === 0 ? (
          <Empty>Nothing is being held back.</Empty>
        ) : (
          <ul className="divide-y divide-border">
            {groups.map((group) => (
              <HeldGroup
                key={group.reason}
                reason={group.reason}
                rows={group.rows}
                data={data}
                now={data.now}
              />
            ))}
          </ul>
        )}
      </div>
    </SectionCard>
  );
}
