import type { ReactNode } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  Bot,
  CalendarClock,
  Check,
  ChevronRight,
  Circle,
  Clock,
  Loader2,
  RotateCw,
  Sparkles,
  Timer,
  X,
} from "lucide-react";

import {
  costQuery,
  fleetQuery,
  goalsQuery,
  queryKeys,
  suggestionsQuery,
  todayAgentQuery,
  todayAgentsQuery,
  todayCronsQuery,
  wrapQuery,
} from "../api/client";
import {
  runCronJob,
  runTodayAgent,
  setSuggestionStatus,
  startTodayAgent,
  stopTodayAgent,
} from "../api/writes";
import type { CatalogItem, DailyGoal, QuarterGoal, Suggestion } from "../api/types";
import { AgentActivity } from "../components/AgentActivity";
import { ConfirmButton } from "../components/ConfirmButton";
import { Empty, Hint, LoadError, SectionCard } from "../components/SectionCard";
import { SteerPanel } from "../components/SteerPanel";
import { StatGrid, StatTile } from "../components/StatTile";
import { fmtAic, fmtUsd, timeAgo } from "../lib/format";
import { pushToast } from "../lib/toast";
import { Badge, type BadgeProps } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import { Card } from "../components/ui/card";
import { Progress } from "../components/ui/progress";
import { ScrollArea } from "../components/ui/scroll-area";
import { Separator } from "../components/ui/separator";
import { Skeleton } from "../components/ui/skeleton";
import { TooltipProvider } from "../components/ui/tooltip";
import { cn } from "../lib/utils";

/** Quarter-goal health → Badge variant (mirrors the CLI's status palette); unknown → muted. */
const STATUS_VARIANT: Record<string, BadgeProps["variant"]> = {
  done: "ok",
  "on-track": "ok",
  "at-risk": "warn",
  "off-track": "urgent",
  unscored: "muted",
};

/** One daily goal as a checklist row: a done/open marker, the text (a link when the goal carries
 * one), the aligned quarter-goal key when set (`bn daily add --q`), and — when the goal reports a
 * percentage — a real bar under it rather than only a number in a chip. */
function GoalRow({ goal }: { goal: DailyGoal }) {
  return (
    <li className="space-y-1">
      <div className="flex items-start gap-2 text-sm">
        <span className="mt-0.5 shrink-0" aria-hidden="true">
          {goal.done ? (
            <Check className="h-4 w-4 text-ok" />
          ) : (
            <Circle className="h-4 w-4 text-muted-foreground" />
          )}
        </span>
        <span className={cn("min-w-0 flex-1", goal.done && "text-muted-foreground line-through")}>
          {goal.link ? (
            <a
              href={goal.link}
              target="_blank"
              rel="noreferrer"
              className="text-primary underline underline-offset-2"
            >
              {goal.text}
            </a>
          ) : (
            goal.text
          )}
        </span>
        {goal.pct != null && !goal.done && (
          <Badge variant="info" className="shrink-0 tabular-nums">
            {goal.pct}%
          </Badge>
        )}
        {goal.q && (
          <Hint label="Aligned quarter goal">
            <span className="shrink-0 font-mono text-xs text-muted-foreground">◇ {goal.q}</span>
          </Hint>
        )}
      </div>
      {goal.pct != null && !goal.done && (
        <Progress value={goal.pct} tone="info" className="ml-6 h-1" />
      )}
    </li>
  );
}

/** One quarter goal: its derived health chip, the goal text, and its key. */
function QuarterRow({ goal }: { goal: QuarterGoal }) {
  return (
    <li className="flex items-start gap-2 text-sm">
      <Badge variant={STATUS_VARIANT[goal.status] ?? "muted"} className="shrink-0">
        {goal.status}
      </Badge>
      <span className="min-w-0 flex-1">{goal.text}</span>
      <Hint label={`Quarter-goal key: ${goal.key}`}>
        <span className="shrink-0 font-mono text-xs text-muted-foreground">{goal.key}</span>
      </Hint>
    </li>
  );
}

/** One suggested task: the proposal text (a link when it carries its source finding url), the
 * rationale + source finding, and accept/dismiss controls that hit `POST /api/suggestions/:id`. */
function SuggestionRow({
  s,
  onReview,
  pending,
}: {
  s: Suggestion;
  onReview: (id: string, status: "accepted" | "dismissed") => void;
  pending: boolean;
}) {
  return (
    <li>
      <Card className="flex items-start gap-2 border-border bg-muted px-3 py-2.5 text-sm shadow-sm transition-colors hover:border-input hover:bg-accent">
        <Sparkles className="mt-0.5 h-4 w-4 shrink-0 text-primary" aria-hidden="true" />
        {/* Cap the text at a readable measure so accept/dismiss stay *next to* the proposal they act
            on. Left to `flex-1` the row stretched the full column and parked the two controls 300px
            away from the sentence, which is a long way to travel to say "no". */}
        <div className="min-w-0 flex-1 space-y-0.5 lg:max-w-[88ch]">
          {s.url ? (
            <a
              href={s.url}
              target="_blank"
              rel="noreferrer"
              className="text-primary underline underline-offset-2"
            >
              {s.title}
            </a>
          ) : (
            <span className="text-foreground/90">{s.title}</span>
          )}
          {s.rationale && <p className="text-xs text-muted-foreground">{s.rationale}</p>}
          <span className="font-mono text-xs text-muted-foreground/80">{s.source}</span>
        </div>
        <div className="flex shrink-0 items-center gap-1">
          <Hint label="Accept — keep this proposal">
            <Button
              variant="ghost"
              size="icon"
              disabled={pending}
              aria-label={`Accept ${s.title}`}
              onClick={() => onReview(s.id, "accepted")}
            >
              <Check className="text-ok" />
            </Button>
          </Hint>
          <Hint label="Dismiss — drop this proposal">
            <Button
              variant="ghost"
              size="icon"
              disabled={pending}
              aria-label={`Dismiss ${s.title}`}
              onClick={() => onReview(s.id, "dismissed")}
            >
              <X className="text-muted-foreground" />
            </Button>
          </Hint>
        </div>
      </Card>
    </li>
  );
}

/** Fire a today cron/agent on demand (#575): `bn run <job>` via `POST /api/cron/run` — the same
 * code path a scheduled timer runs, and the *shared* trigger primitive the Command Center catalog
 * uses. Confirms first, toasts the outcome, and invalidates the today lists (+ the catalog / cron /
 * action feeds) so the row's refreshed last-run health appears without a reload. */
function TriggerButton({ job, label }: { job: string; label: string }) {
  const qc = useQueryClient();
  const mut = useMutation({
    mutationFn: () => runCronJob(job),
    onSuccess: () => pushToast(`Ran ${job}`, "success"),
    onError: (e) => pushToast(`run ${job} failed: ${e.message}`, "error"),
    onSettled: () => {
      void qc.invalidateQueries({ queryKey: queryKeys.todayCrons });
      void qc.invalidateQueries({ queryKey: queryKeys.todayAgents });
      void qc.invalidateQueries({ queryKey: queryKeys.catalog });
      void qc.invalidateQueries({ queryKey: queryKeys.cron });
      void qc.invalidateQueries({ queryKey: queryKeys.actions });
    },
  });

  return (
    <ConfirmButton
      variant="secondary"
      size="sm"
      disabled={mut.isPending}
      title={`${label} ${job} now?`}
      description={
        <>
          Fires <code className="font-mono text-foreground">bn run {job}</code> — the same code path
          the scheduled timer runs.
        </>
      }
      confirmLabel={`${label} it`}
      onConfirm={() => mut.mutate()}
    >
      {mut.isPending ? (
        <>
          <Loader2 className="animate-spin" /> running…
        </>
      ) : (
        <>
          <RotateCw /> {label}
        </>
      )}
    </ConfirmButton>
  );
}

/** One today-automation row (#575) — a scheduled cron or staff agent firing today: its icon +
 * name, its declared cadence, its last-run health, and the shared run-now/trigger control. */
function AutomationRow({ item, now, label }: { item: CatalogItem; now: number; label: string }) {
  const staff = item.kind === "staff-agent";
  const failed = item.ok === false;
  const bar = failed ? "bg-urgent" : item.last_run != null ? "bg-ok" : "bg-border";
  return (
    <li>
      {/* The health rail survives the move to cards: it is the one thing that lets you scan twenty
          of these for the broken one without reading a word. */}
      <Card className="relative overflow-hidden border-border bg-muted py-2 pl-3.5 pr-2 shadow-sm transition-colors hover:border-input hover:bg-accent">
        <span className={cn("absolute inset-y-0 left-0 w-0.5", bar)} aria-hidden="true" />
        <div className="flex flex-wrap items-center gap-2">
          {staff ? (
            <Bot className="h-4 w-4 text-primary" aria-hidden="true" />
          ) : (
            <CalendarClock className="h-4 w-4 text-muted-foreground" aria-hidden="true" />
          )}
          <span className="truncate font-mono text-sm font-medium text-foreground">
            {item.name}
          </span>
          {item.last_run != null && (
            <Hint
              label={
                failed
                  ? `Last run exited ${item.last_exit ?? "?"} — it did not succeed.`
                  : "Last run succeeded."
              }
            >
              <Badge variant={failed ? "urgent" : "ok"}>
                {failed ? `exit ${item.last_exit ?? "?"}` : "ok"}
              </Badge>
            </Hint>
          )}
          <span className="flex-1" />
          <TriggerButton job={item.job} label={label} />
        </div>
        <div className="mt-2 flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-muted-foreground">
          {item.schedule ? (
            <Hint label="Declared schedule">
              <span className="inline-flex items-center gap-1 font-mono">
                <CalendarClock className="h-3.5 w-3.5" /> {item.schedule}
              </span>
            </Hint>
          ) : (
            <Hint label="No declared schedule — this only ever fires on demand.">
              <span className="inline-flex items-center gap-1">
                <Timer className="h-3.5 w-3.5" /> on demand
              </span>
            </Hint>
          )}
          {item.last_run != null ? (
            <Hint label="When it last ran">
              <span className="inline-flex items-center gap-1">
                <Clock className="h-3.5 w-3.5" /> ran {timeAgo(item.last_run, now)} ago
              </span>
            </Hint>
          ) : (
            <span>never run</span>
          )}
          {item.runs > 0 && (
            <span>
              {item.runs} run{item.runs === 1 ? "" : "s"}
              {item.failures > 0 && <span className="text-urgent"> · {item.failures} failed</span>}
            </span>
          )}
          {item.description && <span className="truncate">{item.description}</span>}
        </div>
      </Card>
    </li>
  );
}

/** One automation list — today's crons or today's staff agents. Both are empty-tolerant (a quiet
 * day, or a weekend with no weekday crons) and a failed fetch is said in the urgent tone rather
 * than rendered as an empty list, because "we could not ask" is not "nothing is scheduled". */
function AutomationSection({
  title,
  items,
  isLoading,
  isError,
  now,
  label,
  empty,
  errorText,
}: {
  title: string;
  items: CatalogItem[];
  isLoading: boolean;
  isError: boolean;
  now: number;
  label: string;
  empty: string;
  errorText: string;
}) {
  return (
    <SectionCard title={title} count={isLoading ? undefined : String(items.length)}>
      {isLoading ? (
        <Skeleton className="h-16 w-full" />
      ) : isError ? (
        <LoadError>{errorText}</LoadError>
      ) : items.length === 0 ? (
        <Empty>{empty}</Empty>
      ) : (
        <ul className="space-y-2">
          {items.map((item) => (
            <AutomationRow key={item.job} item={item} now={now} label={label} />
          ))}
        </ul>
      )}
    </SectionCard>
  );
}

/** Today's scheduled automation (#575) — the crons + staff agents whose schedule fires today, each
 * with its last-run status and a run-now/trigger control (the shared `bn run <job>` path). */
function TodayAutomation() {
  const crons = useQuery(todayCronsQuery());
  const agents = useQuery(todayAgentsQuery());
  const now = Math.floor(Date.now() / 1000);

  return (
    <>
      <AutomationSection
        title="Today's crons"
        items={crons.data?.agents ?? []}
        isLoading={crons.isLoading}
        isError={crons.isError}
        now={now}
        label="run"
        empty="No crons scheduled to fire today."
        errorText="Could not load today's crons."
      />
      <AutomationSection
        title="Today's agents"
        items={agents.data?.agents ?? []}
        isLoading={agents.isLoading}
        isError={agents.isError}
        now={now}
        label="trigger"
        empty="No agents scheduled to run today."
        errorText="Could not load today's agents."
      />
    </>
  );
}

/**
 * Today's agent, hosted right here.
 *
 * The agent is a live harness session in the tmux `planning` session — not a one-shot job — so this
 * card is a *window onto a running thing*, not a button. It carries the three affordances that make
 * a hosted agent usable without leaving the page: whether it is alive, what it is doing
 * (`AgentActivity`, the same transcript stream the agent-detail page renders), and a way to talk to
 * it (`SteerPanel`, the same composer → `bn agent send`).
 *
 * Both of those components are reused verbatim rather than reimplemented: `SteerPanel` already
 * knows how to grey itself when the control channel is `none`/`ambiguous`, and getting that wrong
 * here would mean a composer that silently writes into nothing.
 *
 * A recorded-but-dead agent is shown as stopped **with its record still visible** — knowing the
 * 08:30 run died is a fact worth seeing, and it is why `running` comes from the live fleet rather
 * than from the record's own status field.
 */
function TodayAgentCard() {
  const qc = useQueryClient();
  const agent = useQuery(todayAgentQuery());
  const view = agent.data;
  const record = view?.agent ?? null;
  const running = view?.running ?? false;

  const invalidate = () => {
    void qc.invalidateQueries({ queryKey: queryKeys.todayAgent });
    void qc.invalidateQueries({ queryKey: queryKeys.fleet });
  };

  const start = useMutation({
    mutationFn: () => startTodayAgent(),
    onSuccess: () => {
      pushToast("Today's agent starting — it lands in the tmux `planning` session", "success");
      invalidate();
    },
    onError: (e) => pushToast(`Could not start Today's agent: ${e.message}`, "error"),
  });
  const stop = useMutation({
    mutationFn: () => stopTodayAgent(),
    onSuccess: () => {
      pushToast("Today's agent stopped", "success");
      invalidate();
    },
    onError: (e) => pushToast(`Could not stop Today's agent: ${e.message}`, "error"),
  });

  const busy = start.isPending || stop.isPending;

  return (
    <SectionCard
      title={
        <span className="inline-flex items-center gap-1.5">
          <Bot className="h-3.5 w-3.5 text-primary" aria-hidden="true" />
          Today&apos;s agent
        </span>
      }
      count={
        <span className="inline-flex items-center gap-1.5">
          <Badge variant={running ? "ok" : "muted"}>{running ? "running" : "not running"}</Badge>
          {running && view?.health && <Badge variant="info">{view.health}</Badge>}
        </span>
      }
      action={
        running ? (
          <ConfirmButton
            variant="secondary"
            size="sm"
            disabled={busy}
            title="Stop Today's agent?"
            description="Closes the agent's tmux window and releases its lease. The day's record stays — you can start it again to pick back up."
            confirmLabel="Stop it"
            destructive
            onConfirm={() => stop.mutate()}
          >
            {stop.isPending ? (
              <>
                <Loader2 className="animate-spin" /> stopping…
              </>
            ) : (
              <>
                <X /> Stop
              </>
            )}
          </ConfirmButton>
        ) : (
          <Hint label="Launch Today's agent into the tmux `planning` session, seeded with today's findings">
            <Button size="sm" disabled={busy} onClick={() => start.mutate()}>
              {start.isPending ? (
                <>
                  <Loader2 className="animate-spin" /> starting…
                </>
              ) : (
                <>
                  <Sparkles /> Start agent
                </>
              )}
            </Button>
          </Hint>
        )
      }
    >
      {agent.isLoading ? (
        <Skeleton className="h-16 w-full" />
      ) : !record ? (
        <Empty>
          Today's agent hasn't run yet. Starting it opens a live session in the{" "}
          <code className="rounded bg-muted px-1 py-0.5 font-mono text-xs">planning</code> tmux
          session, seeded with what this morning's crons found — then you steer it from here.
        </Empty>
      ) : (
        <div className="space-y-3">
          <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-muted-foreground">
            <Hint label="Steering target — `bn agent send <target>`">
              <span className="font-mono">{record.target}</span>
            </Hint>
            <Separator orientation="vertical" className="h-3" />
            <Hint label="tmux session:window — prefix C-p jumps here">
              <span className="font-mono">{record.tmux_target}</span>
            </Hint>
            <Separator orientation="vertical" className="h-3" />
            <span>{record.harness}</span>
            {!running && (
              <span className="text-warn">
                recorded, but no live session — start it again to pick the day back up
              </span>
            )}
          </div>

          {running && (
            <>
              {/* The live transcript, on Today rather than only on `/agents/:id`. Today's agent is
                  the one session you are supervising all day, so watching it should not cost a
                  navigation away from the day it is working on. */}
              <AgentActivity sessionId={record.session_id} />
              <SteerPanel target={record.target} control={view?.control ?? undefined} />
            </>
          )}
        </div>
      )}
    </SectionCard>
  );
}

/** Suggested tasks (#577) — Today's agent synthesizes the cron findings into reviewable proposals.
 * A manual Run trigger fires `POST /api/today/run`; each proposal can be accepted or dismissed.
 * Shows the open (suggested) list, refreshing the query after every write. */
function SuggestedTasks() {
  const qc = useQueryClient();
  const suggestions = useQuery(suggestionsQuery("suggested"));
  const open = suggestions.data?.suggestions ?? [];

  const invalidate = () =>
    void qc.invalidateQueries({ queryKey: queryKeys.suggestions("suggested") });

  const runMut = useMutation({
    mutationFn: () => runTodayAgent(),
    onSuccess: (r) => {
      pushToast(
        r.report.added > 0
          ? `Today's agent: ${r.report.added} new suggestion${r.report.added === 1 ? "" : "s"}`
          : "Today's agent: no new suggestions",
        "success",
      );
      invalidate();
    },
    onError: (e) => pushToast(`Today's agent failed: ${e.message}`, "error"),
  });

  const reviewMut = useMutation({
    mutationFn: ({ id, status }: { id: string; status: "accepted" | "dismissed" }) =>
      setSuggestionStatus(id, status),
    onSuccess: (r) => {
      pushToast(`Suggestion ${r.status}`, "success");
      invalidate();
    },
    onError: (e) => pushToast(`Review failed: ${e.message}`, "error"),
  });

  return (
    <SectionCard
      eyebrow="Proposals"
      title="Suggested tasks"
      count={open.length > 0 ? String(open.length) : undefined}
      action={
        /* The offline fold, demoted: Today's agent is the hosted session, and this is the
           deterministic re-synthesis from the findings store — useful to refresh the list without
           spending a turn, and what a headless box falls back to. */
        <Hint label="Re-synthesize suggestions from the cron findings (offline, no agent turn)">
          <Button
            variant="ghost"
            size="sm"
            disabled={runMut.isPending}
            onClick={() => runMut.mutate()}
          >
            {runMut.isPending ? (
              <>
                <Loader2 className="animate-spin" /> syncing…
              </>
            ) : (
              <>
                <RotateCw /> Re-synthesize
              </>
            )}
          </Button>
        </Hint>
      }
    >
      {suggestions.isLoading ? (
        <Skeleton className="h-16 w-full" />
      ) : open.length === 0 ? (
        <Empty>
          No suggestions yet — start Today&apos;s agent, or hit{" "}
          <span className="font-medium">Re-synthesize</span> to fold the cron findings in offline.
        </Empty>
      ) : (
        <ul className="space-y-2">
          {open.map((s) => (
            <SuggestionRow
              key={s.id}
              s={s}
              pending={reviewMut.isPending}
              onReview={(id, status) => reviewMut.mutate({ id, status })}
            />
          ))}
        </ul>
      )}
    </SectionCard>
  );
}

/** One scrolling column of the digest. The cards stack with gaps inside it, and the column scrolls
 * independently of its siblings — so reading the day's agent never scrolls the day's goals away. */
function Column({
  label,
  className,
  children,
}: {
  label: string;
  className?: string;
  children: ReactNode;
}) {
  return (
    <ScrollArea aria-label={label} className={cn("h-full border-border", className)}>
      <div className="space-y-3 p-3">{children}</div>
    </ScrollArea>
  );
}

/**
 * `/today` — the Today view (#490, Epic #488 Mission Control). A personal *aimed vs achieved*
 * digest: today's daily goals (with their quarter alignment) and the quarter's goals on the left;
 * the proposals and automation to act on in the middle; the agent working the day — and the day's
 * numbers — on the right. Read-only where it can be: `/api/goals` + `/api/wrap` (+ today's cost).
 *
 * Built from the same card language as `/` (#1006): `SectionCard` bands, rows that are `Card`s,
 * `ConfirmButton`/`AlertDialog` in place of `window.confirm`, `Tooltip` in place of native `title`
 * attributes (which a phone can never show), and `ScrollArea` for the three independent columns.
 */
export function Today() {
  const goals = useQuery(goalsQuery());
  const wrap = useQuery(wrapQuery());
  const cost = useQuery(costQuery(7));
  const fleet = useQuery(fleetQuery());

  const daily = goals.data?.daily;
  const quarter = goals.data?.quarter;
  const w = wrap.data;

  const dailyGoals = daily?.goals ?? [];
  const openGoals = dailyGoals.filter((g) => !g.done);
  const doneGoals = dailyGoals.filter((g) => g.done);
  const totalGoals = dailyGoals.length;
  const doneCount = daily?.done ?? doneGoals.length;

  const mergedPrs = w?.merged_prs ?? [];
  const commits = w?.unnoted_commits ?? [];
  const openTodos = w?.open_todos ?? [];

  // Today's agents/spend. The cost fold's `by_day` only carries *finished, ingested* runs, so an
  // agent burning tokens right now isn't in it yet — mirror /cost and fold in the live fleet
  // (currently-running agents) so the tile reflects reality instead of always reading 0. AIC/$
  // land only once a run finishes and is ingested, so `AIC spent` is today's ingested total (`—`
  // until the first run of the day wraps).
  const todayDate = w?.date ?? daily?.date;
  const todayCost = cost.data?.by_day.find((r) => r.key === todayDate);
  const finishedToday = todayCost?.sessions ?? 0;
  const liveAgents = fleet.data?.active ?? 0;
  const agentsToday = finishedToday + liveAgents;
  const aicToday = todayCost?.aic ?? null;
  // AIC is the *Copilot* meter, so a Claude-only day has no AIC at all and the tile used to read
  // `—` — leaving "what did today cost" unanswered on most days. Fall back to the API-equiv
  // benchmark (and relabel, because the two are not the same unit) whenever there is no AIC.
  const usdToday = todayCost?.cost_usd ?? null;
  const spentIsAic = aicToday !== null;

  const loading = goals.isLoading || wrap.isLoading || cost.isLoading || fleet.isLoading;
  const achievedNothing = doneGoals.length === 0 && mergedPrs.length === 0 && commits.length === 0;
  const goalPct = totalGoals > 0 ? Math.round((doneCount / totalGoals) * 100) : 0;

  return (
    <TooltipProvider delayDuration={250}>
      <div className="flex h-full min-h-0 flex-col">
        {/* The day's identity on one baseline with its date, flush to the top rule — the console's
            header treatment, rather than a document title floating above a stack of cards. */}
        <header className="flex flex-none items-baseline gap-3 border-b border-border px-4 py-2.5">
          <h1 className="font-display text-lg font-semibold tracking-tight text-foreground">
            Today
          </h1>
          <span className="font-mono text-[11px] tabular-nums text-muted-foreground/70">
            {todayDate ?? "—"} · aimed vs achieved
          </span>
        </header>

        {(goals.isError || wrap.isError) && (
          <div className="m-3 rounded-lg border border-urgent/30 bg-urgent-muted p-4 text-sm text-urgent">
            Could not load today&apos;s data.
          </div>
        )}

        {/* Hold the whole aimed-vs-achieved digest until every feeding query resolves — otherwise the
            summary tiles and "Achieved today" count render 0/empty on a cold load, then snap to the
            real values (0 PRs → 11 PRs, etc). OR across the queries, never AND (#544). */}
        {loading ? (
          <div
            className="grid min-h-0 flex-1 grid-cols-1 gap-3 p-3 lg:grid-cols-[20rem_minmax(0,1fr)]"
            aria-hidden="true"
          >
            {[0, 1].map((i) => (
              <div key={i} className="space-y-3">
                <Skeleton className="h-32 w-full" />
                <Skeleton className="h-48 w-full" />
              </div>
            ))}
          </div>
        ) : (
          /* Three columns on a wide screen: the day's intent, the work to act on, and the agent
             working it. Below 1181px the agent column folds *under* the centre column rather than
             disappearing (same element, re-placed), and on a phone the whole thing is one stack. */
          <div
            className={cn(
              "grid min-h-0 flex-1 grid-cols-1",
              // 20rem + 22rem is what the `wide` breakpoint (1181px) was sized against — see the
              // screen's definition in tailwind.config. Widening these without moving the
              // breakpoint starves the centre column at exactly the width that unlocks it.
              "lg:grid-cols-[20rem_minmax(0,1fr)] lg:grid-rows-[minmax(0,1fr)_auto]",
              "wide:grid-cols-[20rem_minmax(0,1fr)_22rem] wide:grid-rows-1",
            )}
          >
            <Column
              label="the day"
              className="lg:col-start-1 lg:row-span-2 lg:row-start-1 lg:border-r wide:row-span-1"
            >
              <SectionCard
                eyebrow="Intent"
                title="Aimed today"
                count={totalGoals > 0 ? `${doneCount}/${totalGoals} done` : undefined}
              >
                {totalGoals > 0 && (
                  <Progress
                    value={goalPct}
                    tone={doneCount >= totalGoals ? "ok" : "primary"}
                    className="mb-3"
                  />
                )}
                {dailyGoals.length === 0 ? (
                  <Empty>
                    No daily goals yet — set them with{" "}
                    <code className="rounded bg-muted px-1 py-0.5 font-mono text-xs">
                      bn daily add
                    </code>
                    .
                  </Empty>
                ) : openGoals.length === 0 ? (
                  <Empty>All of today&apos;s goals are done. 🎉</Empty>
                ) : (
                  <ul className="space-y-2">
                    {openGoals.map((g) => (
                      <GoalRow key={g.id} goal={g} />
                    ))}
                  </ul>
                )}
              </SectionCard>

              {quarter && quarter.goals.length > 0 && (
                <SectionCard
                  eyebrow="Alignment"
                  title={`Quarter ${quarter.quarter}`}
                  count={`${quarter.on_track}/${quarter.total} on track`}
                >
                  {quarter.total > 0 && (
                    <Progress
                      value={Math.round((quarter.on_track / quarter.total) * 100)}
                      tone={quarter.on_track >= quarter.total ? "ok" : "warn"}
                      className="mb-3"
                    />
                  )}
                  <ul className="space-y-2">
                    {quarter.goals.map((g) => (
                      <QuarterRow key={g.key} goal={g} />
                    ))}
                  </ul>
                </SectionCard>
              )}

              <SectionCard
                eyebrow="Achieved"
                title="Achieved today"
                tone={doneGoals.length + mergedPrs.length > 0 ? "ready" : undefined}
                count={
                  doneGoals.length + mergedPrs.length > 0
                    ? String(doneGoals.length + mergedPrs.length)
                    : undefined
                }
              >
                <div className="space-y-3">
                  {doneGoals.length > 0 && (
                    <ul className="space-y-2">
                      {doneGoals.map((g) => (
                        <GoalRow key={g.id} goal={g} />
                      ))}
                    </ul>
                  )}

                  {mergedPrs.length > 0 && (
                    <div className="space-y-1.5">
                      {doneGoals.length > 0 && <Separator />}
                      <div className="text-[0.68rem] uppercase tracking-[0.08em] text-muted-foreground/70">
                        PRs merged
                      </div>
                      <ul className="space-y-2">
                        {mergedPrs.map((pr) => (
                          <li key={pr.number} className="flex items-start gap-2 text-sm">
                            <Check className="mt-0.5 h-4 w-4 shrink-0 text-ok" aria-hidden="true" />
                            <a
                              className="min-w-0 flex-1 text-primary underline underline-offset-2"
                              href={pr.url}
                              target="_blank"
                              rel="noreferrer"
                            >
                              #{pr.number} {pr.title}
                            </a>
                          </li>
                        ))}
                      </ul>
                    </div>
                  )}

                  {commits.length > 0 && (
                    <div className="space-y-1.5">
                      <Separator />
                      <div className="flex items-baseline gap-2 text-[0.68rem] uppercase tracking-[0.08em] text-muted-foreground/70">
                        Commits
                        <span className="font-mono normal-case tracking-normal">→ bn sync</span>
                      </div>
                      <ul className="space-y-2">
                        {commits.map((c) => (
                          <li key={c.hash} className="flex items-start gap-2 text-sm">
                            <code className="shrink-0 rounded bg-muted px-1 py-0.5 font-mono text-xs text-muted-foreground">
                              {c.hash}
                            </code>
                            <span className="min-w-0 flex-1 text-foreground/90">{c.subject}</span>
                          </li>
                        ))}
                      </ul>
                    </div>
                  )}

                  {achievedNothing && wrap.isSuccess && goals.isSuccess && (
                    <Empty>
                      Nothing wrapped up yet — close a goal or merge a PR to fill this in.
                    </Empty>
                  )}
                </div>
              </SectionCard>
            </Column>

            <Column
              label="proposals and open work"
              className="lg:col-start-2 lg:row-start-1 lg:border-r"
            >
              <SuggestedTasks />
              <TodayAutomation />
              {openTodos.length > 0 && (
                <SectionCard title="Open todos" count="across branch notes · rolling forward">
                  <ul className="space-y-2">
                    {openTodos.slice(0, 12).map((t, i) => (
                      <li
                        key={`${t.repo}/${t.branch}/${i}`}
                        className="flex items-start gap-2 text-sm"
                      >
                        <ChevronRight
                          className="mt-0.5 h-4 w-4 shrink-0 text-muted-foreground"
                          aria-hidden="true"
                        />
                        <span className="min-w-0 flex-1 text-foreground/90">{t.text}</span>
                        <span className="shrink-0 font-mono text-xs text-muted-foreground">
                          {t.repo}/{t.branch}
                        </span>
                      </li>
                    ))}
                  </ul>
                  {openTodos.length > 12 && (
                    <p className="mt-2 text-[11px] text-muted-foreground">
                      Showing 12 of {openTodos.length}.
                    </p>
                  )}
                </SectionCard>
              )}
            </Column>

            <Column
              label="today's agent"
              className="border-t lg:col-start-2 lg:row-start-2 wide:col-start-3 wide:row-start-1 wide:border-t-0"
            >
              <TodayAgentCard />

              {/* The day's numbers sit with the agent that moved them, not in a banner overhead.
                  `overflow-hidden` so the tile grid's 1px rules stop at the card's radius. */}
              <Card className="overflow-hidden">
                <StatGrid cols={2}>
                  <StatTile
                    label="goals done"
                    value={`${doneCount}/${totalGoals}`}
                    tone={totalGoals > 0 && doneCount >= totalGoals ? "green" : "grey"}
                  />
                  <StatTile
                    label="PRs merged"
                    value={String(mergedPrs.length)}
                    tone={mergedPrs.length > 0 ? "green" : "grey"}
                  />
                  <Hint
                    label={
                      liveAgents > 0
                        ? `${finishedToday} finished + ${liveAgents} running now`
                        : "Finished, ingested runs today"
                    }
                  >
                    <StatTile
                      label="agents today"
                      value={String(agentsToday)}
                      tone={agentsToday > 0 ? "blue" : "grey"}
                    />
                  </Hint>
                  <Hint
                    label={
                      spentIsAic
                        ? "AI Credits — the metered Copilot bill for today's finished runs (lands once a run is ingested)"
                        : "No Copilot runs today, so there is no AIC bill — this is the cache-aware API-equivalent benchmark for today's finished runs. A benchmark, not a bill."
                    }
                  >
                    <StatTile
                      label={spentIsAic ? "AIC spent" : "$ spent"}
                      value={spentIsAic ? fmtAic(aicToday) : fmtUsd(usdToday)}
                      tone="grey"
                    />
                  </Hint>
                </StatGrid>
              </Card>
            </Column>
          </div>
        )}
      </div>
    </TooltipProvider>
  );
}

export default Today;
