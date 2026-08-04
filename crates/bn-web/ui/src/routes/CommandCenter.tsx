import { useMemo, useState } from "react";
import { Link, useSearchParams } from "react-router-dom";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  AlertTriangle,
  ArrowUpRight,
  Zap,
  Plus,
  ChevronDown,
  Inbox,
  Activity,
  Bot,
  CalendarClock,
  Clock,
  Loader2,
  RotateCw,
  Timer,
  CheckCircle2,
  FileText,
} from "lucide-react";
import {
  actionsQuery,
  catalogQuery,
  completedQuery,
  fleetQuery,
  orchestrateQuery,
  queryKeys,
  reviewsQuery,
} from "../api/client";
import { runCronJob } from "../api/writes";
import type {
  ActionItem,
  ActionKind,
  CatalogItem,
  CompletedItem,
  CompletedKind,
  CronActivity,
} from "../api/types";
import { fleetEntries } from "../lib/fleet";
import { useNow } from "../hooks/useNow";
import { timeAgo } from "../lib/format";
import { pushToast } from "../lib/toast";
import { StartAgentComposer } from "../components/StartAgentComposer";
import { ReviewsInbox } from "../components/ReviewsInbox";
import { ToastStack } from "../components/ToastStack";
import { Badge, type BadgeProps } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import { Card } from "../components/ui/card";
import { Skeleton } from "../components/ui/skeleton";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "../components/ui/tabs";
import { cn } from "../lib/utils";

/** Map a severity to a chip variant + the row's left accent bar colour. */
const SEVERITY: Record<string, { variant: BadgeProps["variant"]; bar: string }> = {
  urgent: { variant: "urgent", bar: "bg-urgent" },
  warn: { variant: "warn", bar: "bg-warn" },
  info: { variant: "info", bar: "bg-info" },
};
const severityOf = (s: string) => SEVERITY[s] ?? { variant: "muted" as const, bar: "bg-border" };

/** Short human label per action kind for the filter chips + row tags. */
const KIND_LABEL: Record<ActionKind, string> = {
  attention: "Attention",
  "plan-review": "Plan review",
  todo: "Todo",
  blocker: "Blocker",
  "failing-cron": "Cron",
  "stale-pr": "Stale PR",
  goal: "Goal",
  issue: "Issue",
  suggestion: "Suggestion",
};

/** How many todos the collapsed todo section shows before the "show all" expander. Keeps the
 * urgent, pinned signal above the fold while the long backlog stays one click away. */
const TODO_CAP = 6;

/** One actionable row — its signal, context, and the one-click Start-agent CTA. */
function ActionRow({ item, onStart }: { item: ActionItem; onStart: (i: ActionItem) => void }) {
  const [open, setOpen] = useState(false);
  const { variant, bar } = severityOf(item.severity);
  const where = item.repo ? `${item.repo}${item.branch ? `/${item.branch}` : ""}` : null;
  const urgent = item.default_urgency === "urgent";

  return (
    <Card
      className={cn(
        "relative overflow-hidden pl-3 transition-colors hover:border-border/80 hover:bg-accent/40",
        item.stale && "opacity-70",
      )}
    >
      {/* Left accent bar keyed to severity so the list scans at a glance. */}
      <span className={cn("absolute inset-y-0 left-0 w-1", bar)} aria-hidden="true" />
      {/* `justify-start` with a capped text block, not `justify-between`: on a wide screen
          `justify-between` pinned "Start agent" to the far right edge, leaving several hundred px
          of nothing between a todo and the button that acts on it. Capping the measure keeps the
          two together and stops the title running to an unreadable line length. */}
      <div className="flex flex-col gap-3 p-3 sm:flex-row sm:items-start">
        <div className="min-w-0 flex-1 space-y-1.5 lg:max-w-[96ch]">
          <div className="flex flex-wrap items-center gap-2">
            <Badge variant={variant} className="uppercase tracking-wide">
              {KIND_LABEL[item.type]}
            </Badge>
            <span className="font-medium leading-tight text-foreground">{item.title}</span>
            {item.stale && (
              <span className="text-xs font-medium text-muted-foreground">stale</span>
            )}
          </div>
          <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-muted-foreground">
            <span>{item.source}</span>
            {where && <span className="font-mono text-muted-foreground/90">{where}</span>}
            {item.link && (
              <a
                href={String(item.link)}
                target="_blank"
                rel="noreferrer"
                className="inline-flex items-center gap-0.5 text-primary"
                onClick={(e) => e.stopPropagation()}
              >
                open <ArrowUpRight className="h-3 w-3" />
              </a>
            )}
            {item.detail && (
              <button
                type="button"
                className="text-primary"
                onClick={() => setOpen((v) => !v)}
              >
                {open ? "hide detail" : "detail"}
              </button>
            )}
          </div>
          {open && item.detail && (
            <pre className="mt-2 max-h-64 overflow-auto rounded-md border border-border bg-muted/60 p-3 font-mono text-xs text-muted-foreground">
              {item.detail}
            </pre>
          )}
        </div>
        <div className="shrink-0">
          <Button
            variant={urgent ? "default" : "secondary"}
            size="sm"
            onClick={() => onStart(item)}
            title="open the composer pre-filled with a suggested prompt"
            aria-label={`Start agent — ${item.title}`}
          >
            {urgent ? <Zap className="h-4 w-4" /> : <Plus className="h-4 w-4" />}
            Start agent
          </Button>
        </div>
      </div>
    </Card>
  );
}

/** A small labelled stat in the glance strip — a cell in the band, not a floating card. */
function Glance({ n, label, tone }: { n: number; label: string; tone?: string }) {
  return (
    <div className="flex flex-col justify-center gap-1 bg-card px-3.5 py-3">
      <span
        className={cn("font-mono text-[1.35rem] font-semibold leading-none tabular-nums", tone)}
      >
        {n}
      </span>
      <span className="text-[0.68rem] uppercase tracking-[0.08em] text-muted-foreground">
        {label}
      </span>
    </div>
  );
}

/** A filter toggle chip with a live count. */
function FilterChip({
  active,
  label,
  count,
  onClick,
}: {
  active: boolean;
  label: string;
  count: number;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={cn(
        "inline-flex shrink-0 items-center gap-1.5 rounded-full border px-3 py-1 text-sm transition-colors",
        active
          ? "border-primary/40 bg-primary/15 text-foreground"
          : "border-border bg-card text-muted-foreground hover:bg-accent hover:text-foreground",
      )}
    >
      {label}
      <span
        className={cn(
          "rounded-full px-1.5 text-xs tabular-nums",
          active ? "bg-primary/20 text-foreground" : "bg-muted text-muted-foreground",
        )}
      >
        {count}
      </span>
    </button>
  );
}

/** The loading skeleton — a few placeholder rows so the page never flashes an empty body. */
function LoadingSkeleton() {
  return (
    <div className="space-y-2" aria-hidden="true">
      {[0, 1, 2, 3].map((i) => (
        <Card key={i} className="flex items-center justify-between gap-3 p-3 pl-4">
          <div className="flex-1 space-y-2">
            <Skeleton className="h-4 w-2/3" />
            <Skeleton className="h-3 w-1/3" />
          </div>
          <Skeleton className="h-8 w-24" />
        </Card>
      ))}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Automation hub (#557) — the cron-activity feed + triggerable-agent catalog.
// ---------------------------------------------------------------------------

/** How many catalog entries the collapsed roster shows before the "show all" expander. */
const CATALOG_CAP = 8;

/** Re-trigger a cataloged job/agent (#557): fire `bn run <job>` via `POST /api/cron/run` — the same
 * code path a scheduled timer runs — confirming first, toasting the outcome, and invalidating the
 * catalog + action list (its cron-activity feed) + cron registry so refreshed last-run health
 * appears without a reload. */
function TriggerButton({ job }: { job: string }) {
  const qc = useQueryClient();
  const mut = useMutation({
    mutationFn: () => runCronJob(job),
    onSuccess: () => pushToast(`Ran ${job}`, "success"),
    onError: (e) => pushToast(`run ${job} failed: ${(e as Error).message}`, "error"),
    // Either way the run recorded an outcome — refresh so its new last-run health shows.
    onSettled: () => {
      void qc.invalidateQueries({ queryKey: queryKeys.catalog });
      void qc.invalidateQueries({ queryKey: queryKeys.actions });
      void qc.invalidateQueries({ queryKey: queryKeys.cron });
    },
  });
  return (
    <Button
      variant="secondary"
      size="sm"
      disabled={mut.isPending}
      title={`Run \`bn run ${job}\` now`}
      onClick={() => {
        if (window.confirm(`Run ${job} now? Fires \`bn run ${job}\`.`)) mut.mutate();
      }}
    >
      {mut.isPending ? (
        <>
          <Loader2 className="h-4 w-4 animate-spin" /> running…
        </>
      ) : (
        <>
          <RotateCw className="h-4 w-4" /> re-run
        </>
      )}
    </Button>
  );
}

/** One recent-run row in the cron-activity feed — its outcome (ok / exit code), when it ran, and
 * (on failure) its captured output tail behind a toggle. */
function ActivityRow({ item, now }: { item: CronActivity; now: number }) {
  const [open, setOpen] = useState(false);
  const failed = !item.ok;
  return (
    <Card className="relative overflow-hidden p-3 pl-4">
      <span
        className={cn("absolute inset-y-0 left-0 w-1", failed ? "bg-urgent" : "bg-ok")}
        aria-hidden="true"
      />
      <div className="flex flex-wrap items-center gap-2">
        <span className="truncate font-mono text-sm font-medium text-foreground">{item.job}</span>
        <Badge variant={failed ? "urgent" : "ok"}>
          {failed ? `exit ${item.last_exit ?? "?"}` : "ok"}
        </Badge>
        <span className="flex-1" />
        <Badge variant="muted">{item.scope}</Badge>
      </div>
      <div className="mt-2 flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-muted-foreground">
        <span className="inline-flex items-center gap-1" title="last run">
          <Clock className="h-3.5 w-3.5" /> ran {timeAgo(item.last_run, now)} ago
        </span>
        {item.schedule && (
          <span className="inline-flex items-center gap-1 font-mono" title="declared schedule">
            <CalendarClock className="h-3.5 w-3.5" /> {item.schedule}
          </span>
        )}
        {item.runs > 0 && (
          <span>
            {item.runs} run{item.runs === 1 ? "" : "s"}
            {item.failures > 0 && <span className="text-urgent"> · {item.failures} failed</span>}
          </span>
        )}
        {failed && item.last_output && (
          <button type="button" className="text-primary" onClick={() => setOpen((v) => !v)}>
            {open ? "hide output" : "show output"}
          </button>
        )}
      </div>
      {failed && item.last_output && open && (
        <pre
          className="mt-2 max-h-64 overflow-auto rounded-md border border-border bg-muted/60 p-2 font-mono text-xs text-muted-foreground"
          aria-label={`${item.job} last output`}
        >
          {item.last_output}
        </pre>
      )}
    </Card>
  );
}

/** One triggerable agent/job in the catalog — what it is (staff agent vs cron), its cadence + last
 * run, its metadata behind a detail toggle, and a re-trigger button. */
function CatalogRow({ item, now }: { item: CatalogItem; now: number }) {
  const [open, setOpen] = useState(false);
  const staff = item.kind === "staff-agent";
  const failed = item.ok === false;
  const bar = failed ? "bg-urgent" : item.last_run != null ? "bg-ok" : "bg-border";
  return (
    <Card className="relative overflow-hidden p-3 pl-4">
      <span className={cn("absolute inset-y-0 left-0 w-1", bar)} aria-hidden="true" />
      <div className="flex flex-wrap items-center gap-2">
        {staff ? (
          <Bot className="h-4 w-4 text-primary" aria-hidden="true" />
        ) : (
          <CalendarClock className="h-4 w-4 text-muted-foreground" aria-hidden="true" />
        )}
        <span className="truncate font-mono text-sm font-medium text-foreground">{item.name}</span>
        <Badge variant={staff ? "info" : "muted"}>{staff ? "agent" : "cron"}</Badge>
        {failed && <Badge variant="urgent">exit {item.last_exit ?? "?"}</Badge>}
        <span className="flex-1" />
        <TriggerButton job={item.job} />
      </div>
      <div className="mt-2 flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-muted-foreground">
        {item.schedule ? (
          <span className="inline-flex items-center gap-1 font-mono" title="declared schedule">
            <CalendarClock className="h-3.5 w-3.5" /> {item.schedule}
          </span>
        ) : (
          <span className="inline-flex items-center gap-1" title="no declared schedule">
            <Timer className="h-3.5 w-3.5" /> {item.on_demand ? "on demand" : "no schedule"}
          </span>
        )}
        {item.last_run != null && (
          <span className="inline-flex items-center gap-1" title="last run">
            <Clock className="h-3.5 w-3.5" /> ran {timeAgo(item.last_run, now)} ago
          </span>
        )}
        {item.last_run == null && <span>never run</span>}
        {(item.description != null || staff) && (
          <button type="button" className="text-primary" onClick={() => setOpen((v) => !v)}>
            {open ? "hide detail" : "detail"}
          </button>
        )}
      </div>
      {open && (
        <div className="mt-2 space-y-1.5 rounded-md border border-border bg-muted/40 p-3 text-xs text-muted-foreground">
          {item.description && <p className="text-foreground/90">{item.description}</p>}
          <div className="flex flex-wrap gap-x-4 gap-y-1">
            <span>
              scope <span className="font-mono text-foreground/80">{item.scope}</span>
            </span>
            {staff && item.skill && (
              <span>
                skill <span className="font-mono text-foreground/80">{item.skill}</span>
              </span>
            )}
            {staff && item.model && (
              <span>
                model <span className="font-mono text-foreground/80">{item.model}</span>
              </span>
            )}
            {staff && item.harness && (
              <span>
                harness <span className="font-mono text-foreground/80">{item.harness}</span>
              </span>
            )}
            <span>
              {item.runs} run{item.runs === 1 ? "" : "s"}
              {item.failures > 0 && <span className="text-urgent"> · {item.failures} failed</span>}
            </span>
            {item.declared && <span>registered</span>}
            {item.has_script && <span>script</span>}
          </div>
        </div>
      )}
    </Card>
  );
}

/** The **Automation** hub (#557): the observational cron-activity feed (recent runs, ok + failed)
 * plus the catalog of every triggerable agent/job, each re-triggerable in place. Its own section
 * below the "needs you" list so driving + observing automated work lives in one place. */
function AutomationSection({ activity, now }: { activity: CronActivity[]; now: number }) {
  const catalog = useQuery(catalogQuery());
  const [showAll, setShowAll] = useState(false);
  const roster = catalog.data?.agents ?? [];
  const visible = showAll ? roster : roster.slice(0, CATALOG_CAP);

  return (
    <section className="space-y-4 pt-2">
      <div className="flex items-center gap-2">
        <Activity className="h-4 w-4 text-muted-foreground" />
        <h3 className="text-sm font-semibold uppercase tracking-wide text-muted-foreground">
          Automation
        </h3>
      </div>

      <div className="space-y-2">
        <p className="text-xs text-muted-foreground">Recent cron activity</p>
        {activity.length === 0 ? (
          <Card className="px-4 py-6 text-center text-sm text-muted-foreground">
            No cron runs recorded yet.
          </Card>
        ) : (
          <div className="space-y-2">
            {activity.slice(0, 6).map((a) => (
              <ActivityRow key={`${a.scope}:${a.job}`} item={a} now={now} />
            ))}
          </div>
        )}
      </div>

      <div className="space-y-2">
        <div className="flex items-center gap-2">
          <p className="text-xs text-muted-foreground">Triggerable agents &amp; jobs</p>
          {roster.length > 0 && (
            <span className="text-xs text-muted-foreground/70">{roster.length}</span>
          )}
        </div>
        {catalog.isLoading && (
          <div className="space-y-2" aria-hidden="true">
            {[0, 1, 2].map((i) => (
              <Card key={i} className="space-y-2 p-3 pl-4">
                <Skeleton className="h-4 w-1/2" />
                <Skeleton className="h-3 w-1/3" />
              </Card>
            ))}
          </div>
        )}
        {catalog.isError && (
          <Card className="flex items-center gap-2 border-urgent/30 bg-urgent-muted p-4 text-sm text-urgent">
            <AlertTriangle className="h-4 w-4" />
            Could not load the agent catalog.
          </Card>
        )}
        {!catalog.isLoading && !catalog.isError && roster.length === 0 && (
          <Card className="px-4 py-6 text-center text-sm text-muted-foreground">
            No triggerable agents or jobs on this machine yet.
          </Card>
        )}
        {visible.length > 0 && (
          <div className="grid grid-cols-1 gap-2 sm:grid-cols-2">
            {visible.map((c) => (
              <CatalogRow key={`${c.scope}:${c.job}`} item={c} now={now} />
            ))}
          </div>
        )}
        {roster.length > CATALOG_CAP && (
          <Button
            variant="ghost"
            size="sm"
            className="text-muted-foreground"
            onClick={() => setShowAll((v) => !v)}
          >
            <ChevronDown
              className={cn("h-4 w-4 transition-transform", showAll && "rotate-180")}
            />
            {showAll ? "show fewer" : `show all (${roster.length})`}
          </Button>
        )}
      </div>
    </section>
  );
}

/** Short human label per completed kind for the group tags. */
const COMPLETED_KIND_LABEL: Record<CompletedKind, string> = {
  todo: "Todo",
  goal: "Goal",
  attention: "Attention",
  "merged-pr": "Merged",
};

/** Epoch-seconds → a short local date (`Jul 15`), or empty when the stamp is missing/unparseable. */
function completedDate(epoch: number): string {
  if (!epoch) return "";
  return new Date(epoch * 1000).toLocaleDateString(undefined, {
    month: "short",
    day: "numeric",
  });
}

/** One finished-task row — its kind, provenance, completion date, and the local/remote note jumps
 * that trace it back to its full context (#561). */
function CompletedRow({ item }: { item: CompletedItem }) {
  const [open, setOpen] = useState(false);
  const where = item.repo ? `${item.repo}${item.branch ? `/${item.branch}` : ""}` : null;
  const when = completedDate(item.completed);
  const localTo =
    item.repo && item.branch
      ? `/notes?repo=${encodeURIComponent(item.repo)}&branch=${encodeURIComponent(item.branch)}`
      : null;

  return (
    <Card className="relative overflow-hidden pl-3 transition-colors hover:border-border/80 hover:bg-accent/40">
      <span className="absolute inset-y-0 left-0 w-1 bg-ok" aria-hidden="true" />
      <div className="flex flex-col gap-2 p-3">
        <div className="flex flex-wrap items-center gap-2">
          <Badge variant="muted" className="uppercase tracking-wide">
            {COMPLETED_KIND_LABEL[item.type]}
          </Badge>
          <span className="min-w-0 flex-1 truncate font-medium leading-tight text-foreground">
            {item.title}
          </span>
          {when && (
            <span className="shrink-0 text-xs tabular-nums text-muted-foreground">{when}</span>
          )}
        </div>
        <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-muted-foreground">
          <span>{item.source}</span>
          {where && <span className="font-mono text-muted-foreground/90">{where}</span>}
          {item.link && (
            <a
              href={String(item.link)}
              target="_blank"
              rel="noreferrer"
              className="inline-flex items-center gap-0.5 text-primary"
            >
              open <ArrowUpRight className="h-3 w-3" />
            </a>
          )}
          {/* Note jumps: open the branch note locally (in-app) and/or in the synced remote. */}
          {localTo && (
            <Link
              to={localTo}
              className="inline-flex items-center gap-0.5 text-primary"
              title={item.note.local_path ?? "open the branch note on this machine"}
            >
              <FileText className="h-3 w-3" /> note (local)
            </Link>
          )}
          {item.note.remote_url && (
            <a
              href={item.note.remote_url}
              target="_blank"
              rel="noreferrer"
              className="inline-flex items-center gap-0.5 text-primary"
              title="open the branch note in the synced remote"
            >
              note (remote) <ArrowUpRight className="h-3 w-3" />
            </a>
          )}
          {item.detail && (
            <button type="button" className="text-primary" onClick={() => setOpen((v) => !v)}>
              {open ? "hide detail" : "detail"}
            </button>
          )}
        </div>
        {open && item.detail && (
          <pre className="mt-1 max-h-64 overflow-auto rounded-md border border-border bg-muted/60 p-3 font-mono text-xs text-muted-foreground">
            {item.detail}
          </pre>
        )}
      </div>
    </Card>
  );
}

/** The **Completed** tab body (#561) — the finished-work history, most-recent-first, each row
 * linking back to its branch note locally + remotely. */
function CompletedList() {
  const completed = useQuery(completedQuery());
  const items = completed.data?.completed ?? [];

  if (completed.isLoading) return <LoadingSkeleton />;
  if (completed.isError) {
    return (
      <Card className="flex items-center gap-2 border-urgent/30 bg-urgent-muted p-4 text-sm text-urgent">
        <AlertTriangle className="h-4 w-4" />
        Could not load the completed list.
      </Card>
    );
  }
  if (items.length === 0) {
    return (
      <Card className="flex flex-col items-center gap-2 px-6 py-12 text-center">
        <CheckCircle2 className="h-8 w-8 text-muted-foreground" />
        <h3 className="text-lg font-semibold">Nothing completed yet</h3>
        <p className="max-w-sm text-sm text-muted-foreground">
          Finished todos, achieved goals, resolved attention, and merged work land here.
        </p>
      </Card>
    );
  }
  return (
    <div className="space-y-2">
      {items.map((it) => (
        <CompletedRow key={it.id} item={it} />
      ))}
    </div>
  );
}

/**
 * `/` — the **action-centric command center** (#522), the dashboard's heart. Every actionable
 * signal bn tracks — attention items, plan-reviews, open todos, blockers, failing crons, stale
 * in-flight PRs, today's goals — folds into one ranked list (`GET /api/actions`). From any row,
 * one click opens the [`StartAgentComposer`] pre-filled with a per-type suggested prompt the owner
 * edits before launching an agent (urgent → dispatch, or queue). A glance strip up top pins the
 * live fleet + "needs you" count; a filter row narrows by kind. A second **Reviews** tab folds in
 * the plan-review inbox (#558) — the standalone `/reviews` page retired into this hub, `?tab=reviews`
 * deep-links straight to it. An **Automation** hub (#557) below the actions list shows recent cron
 * activity + the catalog of every triggerable agent/job, each re-triggerable.
 */
export function CommandCenter() {
  const now = useNow();
  const actions = useQuery(actionsQuery());
  const fleet = useQuery(fleetQuery());
  const orch = useQuery(orchestrateQuery());
  const reviews = useQuery(reviewsQuery());
  const [composing, setComposing] = useState<ActionItem | null>(null);
  const [filter, setFilter] = useState<ActionKind | "all" | "urgent">("all");
  const [showAllTodos, setShowAllTodos] = useState(false);
  const [searchParams, setSearchParams] = useSearchParams();
  const tabParam = searchParams.get("tab");
  const tab = tabParam === "reviews" || tabParam === "completed" ? tabParam : "actions";

  const items = actions.data?.actions ?? [];
  const cronActivity = actions.data?.cron_activity ?? [];
  const running = fleetEntries(orch.data, fleet.data).length;
  const urgentCount = items.filter((i) => i.severity === "urgent").length;
  const pendingReviews = (reviews.data?.reviews ?? []).filter((r) => r.status === "pending").length;

  // The kinds actually present, for the filter row (keeps it honest to the current state).
  const kindsPresent = useMemo(() => {
    const set = new Set<ActionKind>();
    for (const i of items) set.add(i.type);
    return [...set];
  }, [items]);

  const shown = items.filter((i) => {
    if (filter === "all") return true;
    if (filter === "urgent") return i.severity === "urgent";
    return i.type === filter;
  });

  // Split the ranked list into the pinned, urgent-first signal (blockers, attention, reviews,
  // crons, stale PRs, goals) and the todo backlog. Todos are already sorted fresh-first with the
  // stale tail last, so we render the pinned rows in full and cap the todos behind an expander —
  // keeping "everything that needs you" honest without burying urgent work under a long todo list.
  const pinned = shown.filter((i) => i.type !== "todo");
  const todos = shown.filter((i) => i.type === "todo");
  const visibleTodos = showAllTodos ? todos : todos.slice(0, TODO_CAP);
  const hiddenTodoCount = todos.length - visibleTodos.length;

  return (
    <Tabs
      value={tab}
      onValueChange={(v) => setSearchParams(v === "actions" ? {} : { tab: v }, { replace: true })}
      className="flex h-full min-h-0 flex-col"
    >
      <header className="flex flex-none flex-wrap items-center gap-x-3 gap-y-2 border-b border-border px-4 py-2.5">
        <h1 className="font-display text-lg font-semibold tracking-tight text-foreground">
          Command center
        </h1>
        <span className="hidden font-mono text-[11px] text-muted-foreground/70 lg:inline">
          everything that needs you — one click to an agent
        </span>
        <TabsList className="ml-1">
          <TabsTrigger value="actions">Actions</TabsTrigger>
          <TabsTrigger value="reviews">
            Reviews
            {pendingReviews > 0 && (
              <Badge variant="warn" className="ml-1.5">
                {pendingReviews}
              </Badge>
            )}
          </TabsTrigger>
          <TabsTrigger value="completed">Completed</TabsTrigger>
        </TabsList>
      </header>

      <div className="min-h-0 flex-1 overflow-y-auto px-4 py-4">
        {/* The action list is a column of rows, so it takes a measure rather than the full bleed:
            at 1900px each row left ~740px of nothing to the right of its button, and three stat
            cells stretched to 560px each to hold a two-digit number. Full-bleed is right for the
            grids and the master/detail screens; it is not right for a single stack of rows. */}
        <TabsContent value="actions" className="mt-0 max-w-[76rem] space-y-5">
          {/* A band of cells divided by the grid's own background, not three floating cards. */}
          <div className="grid auto-rows-fr grid-cols-3 gap-px border-y border-border bg-border">
        <Glance n={items.length} label="actionable" />
        <Glance n={urgentCount} label="urgent" tone={urgentCount > 0 ? "text-urgent" : undefined} />
        <Glance n={running} label="running" tone={running > 0 ? "text-ok" : undefined} />
      </div>

      <div
        className="-mx-1 flex gap-2 overflow-x-auto px-1 pb-1"
        role="group"
        aria-label="filter actions"
      >
        <FilterChip
          active={filter === "all"}
          label="All"
          count={items.length}
          onClick={() => setFilter("all")}
        />
        <FilterChip
          active={filter === "urgent"}
          label="Urgent"
          count={urgentCount}
          onClick={() => setFilter("urgent")}
        />
        {kindsPresent.map((k) => (
          <FilterChip
            key={k}
            active={filter === k}
            label={KIND_LABEL[k]}
            count={items.filter((i) => i.type === k).length}
            onClick={() => setFilter(k)}
          />
        ))}
      </div>

      {actions.isLoading && <LoadingSkeleton />}

      {actions.isError && (
        <Card className="flex items-center gap-2 border-urgent/30 bg-urgent-muted p-4 text-sm text-urgent">
          <AlertTriangle className="h-4 w-4" />
          Could not load the action list.
        </Card>
      )}

      {!actions.isLoading && !actions.isError && shown.length === 0 && (
        <Card className="flex flex-col items-center gap-2 px-6 py-12 text-center">
          <Inbox className="h-8 w-8 text-muted-foreground" />
          <h3 className="text-lg font-semibold">All clear</h3>
          <p className="max-w-sm text-sm text-muted-foreground">
            Nothing needs you right now. New attention items, reviews, and stale PRs land here.
          </p>
        </Card>
      )}

      {pinned.length > 0 && (
        <div className="space-y-2">
          {pinned.map((it) => (
            <ActionRow key={it.id} item={it} onStart={setComposing} />
          ))}
        </div>
      )}

      {todos.length > 0 && (
        <section className="space-y-2">
          <div className="flex items-center gap-2 pt-1">
            <h3 className="text-sm font-semibold uppercase tracking-wide text-muted-foreground">
              Todos
            </h3>
            <span className="text-xs text-muted-foreground">{todos.length}</span>
          </div>
          <div className="space-y-2">
            {visibleTodos.map((it) => (
              <ActionRow key={it.id} item={it} onStart={setComposing} />
            ))}
          </div>
          {todos.length > TODO_CAP && (
            <Button
              variant="ghost"
              size="sm"
              className="text-muted-foreground"
              onClick={() => setShowAllTodos((v) => !v)}
            >
              <ChevronDown
                className={cn("h-4 w-4 transition-transform", showAllTodos && "rotate-180")}
              />
              {showAllTodos ? "show fewer" : `show all (${todos.length})`}
              {!showAllTodos && hiddenTodoCount > 0 && (
                <span className="text-muted-foreground/70">
                  · {hiddenTodoCount} more, oldest first
                </span>
              )}
            </Button>
          )}
        </section>
      )}

          {!actions.isLoading && !actions.isError && (
            <AutomationSection activity={cronActivity} now={now} />
          )}
        </TabsContent>

        <TabsContent value="reviews" className="mt-0">
          <ReviewsInbox />
        </TabsContent>

        <TabsContent value="completed" className="mt-0">
          <CompletedList />
        </TabsContent>
      </div>

      {composing && <StartAgentComposer item={composing} onClose={() => setComposing(null)} />}
      <ToastStack />
    </Tabs>
  );
}
