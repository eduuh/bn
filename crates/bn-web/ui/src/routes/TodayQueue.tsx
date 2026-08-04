import { useMemo, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import type { LucideIcon } from "lucide-react";
import { Bot, CalendarClock, Clock, Microscope, Search, Sparkles, Timer } from "lucide-react";

import { fleetQuery, todayAgentsQuery, todayCronsQuery } from "../api/client";
import type { CatalogItem, LiveAgent } from "../api/types";
import { Badge, type BadgeProps } from "../components/ui/badge";
import { Card } from "../components/ui/card";
import { Skeleton } from "../components/ui/skeleton";
import { timeAgo } from "../lib/format";
import { cn } from "../lib/utils";

/**
 * `Today's queue` tab — everything the automation is doing or about to do today, in one place: the
 * live fleet (what's running right now), plus the crons and staff agents whose schedule fires today.
 *
 * It reads the three feeds the Today page already polls (`/api/fleet`, `/api/today/crons`,
 * `/api/today/agents`) rather than a bespoke endpoint, so the counts here can never disagree with
 * the deck's. Staff-agent rows are coloured by the *skill* they run — an `enhance` (improve),
 * `investigate` or `research` scout reads differently from a plain agent — the same provenance the
 * redesign leans on everywhere else.
 */
type Kind = "cron" | "enhance" | "investigate" | "research" | "agent";

const KIND: Record<Kind, { label: string; Icon: LucideIcon; cls: string }> = {
  cron: { label: "cron", Icon: CalendarClock, cls: "text-muted-foreground" },
  enhance: { label: "enhance", Icon: Sparkles, cls: "text-primary" },
  investigate: { label: "investigate", Icon: Search, cls: "text-info" },
  research: { label: "research", Icon: Microscope, cls: "text-warn" },
  agent: { label: "agent", Icon: Bot, cls: "text-ok" },
};

const FILTERS: Array<{ key: "all" | Kind; label: string }> = [
  { key: "all", label: "All" },
  { key: "cron", label: "Cron" },
  { key: "enhance", label: "Enhance" },
  { key: "investigate", label: "Investigate" },
  { key: "research", label: "Research" },
  { key: "agent", label: "Agents" },
];

/** A staff agent's queue kind is the skill it runs; a cron is a cron; everything else is an agent. */
function skillKind(skill?: string | null): Kind {
  if (skill === "research") return "research";
  if (skill === "investigate" || skill === "plan-investigate") return "investigate";
  if (skill === "improve" || skill === "enhance") return "enhance";
  return "agent";
}

/** A catalog entry (cron or staff agent) → its queue kind. */
function catalogKind(item: CatalogItem): Kind {
  return item.kind === "staff-agent" ? skillKind(item.skill) : "cron";
}

function healthVariant(h: string): BadgeProps["variant"] {
  if (h === "stuck") return "urgent";
  if (h === "idle") return "warn";
  return "ok";
}

function KindChip({ kind }: { kind: Kind }) {
  const k = KIND[kind];
  return (
    <span className={cn("inline-flex shrink-0 items-center gap-1 font-mono text-[11px]", k.cls)}>
      <k.Icon className="h-3.5 w-3.5" aria-hidden="true" /> {k.label}
    </span>
  );
}

function SectionHead({ title, count }: { title: string; count: number | string }) {
  return (
    <div className="flex items-center gap-2">
      <h3 className="font-mono text-[11px] font-semibold uppercase tracking-wide text-muted-foreground">
        {title}
      </h3>
      <span className="font-mono text-[11px] tabular-nums text-muted-foreground/70">{count}</span>
    </div>
  );
}

/** A live agent — a thing happening right now. */
function RunningRow({ a }: { a: LiveAgent }) {
  return (
    <li className="flex flex-wrap items-center gap-x-2.5 gap-y-1 border-b border-border/60 py-1.5 last:border-b-0">
      <KindChip kind="agent" />
      <span className="min-w-0 flex-1 truncate font-mono text-sm text-foreground">{a.target}</span>
      <span className="font-mono text-[11px] text-muted-foreground">{a.harness}</span>
      <Badge variant={healthVariant(a.health)}>{a.health}</Badge>
      <span className="font-mono text-[11px] text-muted-foreground">
        idle {Math.round(a.idle_secs / 60)}m
      </span>
    </li>
  );
}

/** A scheduled cron or staff agent — with its last-run health and declared cadence. */
function ScheduledRow({ item, now }: { item: CatalogItem; now: number }) {
  const kind = catalogKind(item);
  const failed = item.ok === false;
  return (
    <li className="flex flex-wrap items-center gap-x-2.5 gap-y-1 border-b border-border/60 py-1.5 last:border-b-0">
      <KindChip kind={kind} />
      <span className="min-w-0 flex-1 truncate font-mono text-sm text-foreground">{item.name}</span>
      {item.schedule ? (
        <span className="inline-flex items-center gap-1 font-mono text-[11px] text-muted-foreground">
          <CalendarClock className="h-3.5 w-3.5" aria-hidden="true" /> {item.schedule}
        </span>
      ) : (
        <span className="inline-flex items-center gap-1 font-mono text-[11px] text-muted-foreground">
          <Timer className="h-3.5 w-3.5" aria-hidden="true" /> on demand
        </span>
      )}
      {item.last_run != null ? (
        <span className="inline-flex items-center gap-1 font-mono text-[11px] text-muted-foreground">
          <Clock className="h-3.5 w-3.5" aria-hidden="true" /> {timeAgo(item.last_run, now)} ago
        </span>
      ) : (
        <span className="font-mono text-[11px] text-muted-foreground">never run</span>
      )}
      {item.last_run != null ? (
        <Badge variant={failed ? "urgent" : "ok"}>
          {failed ? `exit ${item.last_exit ?? "?"}` : "ok"}
        </Badge>
      ) : (
        <Badge variant="muted">scheduled</Badge>
      )}
    </li>
  );
}

export function TodayQueue() {
  const fleet = useQuery(fleetQuery());
  const crons = useQuery(todayCronsQuery());
  const agents = useQuery(todayAgentsQuery());
  const now = Math.floor(Date.now() / 1000);

  const [filter, setFilter] = useState<"all" | Kind>("all");

  const running = fleet.data?.agents ?? [];
  const scheduled = useMemo<CatalogItem[]>(
    () => [...(crons.data?.agents ?? []), ...(agents.data?.agents ?? [])],
    [crons.data, agents.data],
  );

  // A live agent is always kind `agent`; a scheduled item is coloured by its own kind/skill.
  const runningShown = filter === "all" || filter === "agent" ? running : [];
  const scheduledShown =
    filter === "all" ? scheduled : scheduled.filter((i) => catalogKind(i) === filter);

  const loading = fleet.isLoading || crons.isLoading || agents.isLoading;

  return (
    <div className="space-y-4 pb-8">
      <div className="flex flex-wrap items-center gap-1.5">
        {FILTERS.map((f) => (
          <button
            key={f.key}
            type="button"
            onClick={() => setFilter(f.key)}
            className={cn(
              "rounded-full border px-2.5 py-1 font-mono text-[11px] transition-colors",
              filter === f.key
                ? "border-border-strong bg-secondary text-foreground"
                : "border-border text-muted-foreground hover:text-foreground",
            )}
          >
            {f.label}
          </button>
        ))}
      </div>

      {loading ? (
        <Skeleton className="h-40 w-full" />
      ) : (
        <>
          <Card className="space-y-2 p-3">
            <SectionHead title="Running now" count={runningShown.length} />
            {runningShown.length === 0 ? (
              <p className="py-1.5 text-sm text-muted-foreground">Nothing is running right now.</p>
            ) : (
              <ul>
                {runningShown.map((a) => (
                  <RunningRow key={a.session_id} a={a} />
                ))}
              </ul>
            )}
          </Card>

          <Card className="space-y-2 p-3">
            <SectionHead title="Scheduled today" count={scheduledShown.length} />
            {scheduledShown.length === 0 ? (
              <p className="py-1.5 text-sm text-muted-foreground">
                Nothing scheduled to fire today.
              </p>
            ) : (
              <ul>
                {scheduledShown.map((item) => (
                  <ScheduledRow key={`${item.kind}:${item.job}`} item={item} now={now} />
                ))}
              </ul>
            )}
          </Card>
        </>
      )}

      {(fleet.isError || crons.isError || agents.isError) && (
        <Card className="border-urgent/30 bg-urgent-muted p-4 text-sm text-urgent">
          Could not load today's queue.
        </Card>
      )}
    </div>
  );
}

export default TodayQueue;
