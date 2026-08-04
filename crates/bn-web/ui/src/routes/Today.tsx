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
import { SteerPanel } from "../components/SteerPanel";
import { fmtAic, fmtUsd, timeAgo } from "../lib/format";
import { pushToast } from "../lib/toast";
import { StatGrid, StatTile } from "../components/StatTile";
import { Badge, type BadgeProps } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import { Skeleton } from "../components/ui/skeleton";
import { cn } from "../lib/utils";
import { Pane, PaneRule, Section } from "../components/ui/pane";

/** Quarter-goal health → Badge variant (mirrors the CLI's status palette); unknown → muted. */
const STATUS_VARIANT: Record<string, BadgeProps["variant"]> = {
  done: "ok",
  "on-track": "ok",
  "at-risk": "warn",
  "off-track": "urgent",
  unscored: "muted",
};

/** One daily goal as a checklist row: a done/open marker, the text (a link when the goal carries
 * one), an optional % chip, and the aligned quarter-goal key when set (`bn daily add --q`). */
function GoalRow({ goal }: { goal: DailyGoal }) {
  return (
    <li className="flex items-start gap-2 text-sm">
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
        <span
          className="shrink-0 font-mono text-xs text-muted-foreground"
          title="aligned quarter goal"
        >
          ◇ {goal.q}
        </span>
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
      <span className="shrink-0 font-mono text-xs text-muted-foreground">{goal.key}</span>
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
    <li className="flex items-start gap-2 px-3.5 py-2.5 text-sm hover:bg-muted/40">
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
        <Button
          variant="ghost"
          size="icon"
          disabled={pending}
          title="Accept"
          aria-label={`Accept ${s.title}`}
          onClick={() => onReview(s.id, "accepted")}
        >
          <Check className="h-4 w-4 text-ok" />
        </Button>
        <Button
          variant="ghost"
          size="icon"
          disabled={pending}
          title="Dismiss"
          aria-label={`Dismiss ${s.title}`}
          onClick={() => onReview(s.id, "dismissed")}
        >
          <X className="h-4 w-4 text-muted-foreground" />
        </Button>
      </div>
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
    onError: (e) => pushToast(`run ${job} failed: ${(e as Error).message}`, "error"),
    onSettled: () => {
      void qc.invalidateQueries({ queryKey: queryKeys.todayCrons });
      void qc.invalidateQueries({ queryKey: queryKeys.todayAgents });
      void qc.invalidateQueries({ queryKey: queryKeys.catalog });
      void qc.invalidateQueries({ queryKey: queryKeys.cron });
      void qc.invalidateQueries({ queryKey: queryKeys.actions });
    },
  });
  return (
    <Button
      variant="secondary"
      size="sm"
      disabled={mut.isPending}
      title={`Run \`bn run ${job}\` now`}
      onClick={() => {
        if (window.confirm(`${label} ${job} now? Fires \`bn run ${job}\`.`)) mut.mutate();
      }}
    >
      {mut.isPending ? (
        <>
          <Loader2 className="h-4 w-4 animate-spin" /> running…
        </>
      ) : (
        <>
          <RotateCw className="h-4 w-4" /> {label}
        </>
      )}
    </Button>
  );
}

/** One today-automation row (#575) — a scheduled cron or staff agent firing today: its icon +
 * name, its declared cadence, its last-run health, and the shared run-now/trigger control. */
function AutomationRow({
  item,
  now,
  label,
}: {
  item: CatalogItem;
  now: number;
  label: string;
}) {
  const staff = item.kind === "staff-agent";
  const failed = item.ok === false;
  const bar = failed ? "bg-urgent" : item.last_run != null ? "bg-ok" : "bg-border";
  return (
    // A row with a health rail, not a card: twenty of these stack in one column, and twenty
    // bordered rounded rectangles is twenty edges competing with the pane's own.
    <div className="relative overflow-hidden rounded-sm bg-muted/25 py-2 pl-3 pr-2 hover:bg-muted/45">
      <span className={cn("absolute inset-y-0 left-0 w-0.5", bar)} aria-hidden="true" />
      <div className="flex flex-wrap items-center gap-2">
        {staff ? (
          <Bot className="h-4 w-4 text-primary" aria-hidden="true" />
        ) : (
          <CalendarClock className="h-4 w-4 text-muted-foreground" aria-hidden="true" />
        )}
        <span className="truncate font-mono text-sm font-medium text-foreground">{item.name}</span>
        {item.last_run != null && (
          <Badge variant={failed ? "urgent" : "ok"}>
            {failed ? `exit ${item.last_exit ?? "?"}` : "ok"}
          </Badge>
        )}
        <span className="flex-1" />
        <TriggerButton job={item.job} label={label} />
      </div>
      <div className="mt-2 flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-muted-foreground">
        {item.schedule ? (
          <span className="inline-flex items-center gap-1 font-mono" title="declared schedule">
            <CalendarClock className="h-3.5 w-3.5" /> {item.schedule}
          </span>
        ) : (
          <span className="inline-flex items-center gap-1" title="no declared schedule">
            <Timer className="h-3.5 w-3.5" /> on demand
          </span>
        )}
        {item.last_run != null ? (
          <span className="inline-flex items-center gap-1" title="last run">
            <Clock className="h-3.5 w-3.5" /> ran {timeAgo(item.last_run, now)} ago
          </span>
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
    </div>
  );
}

/** Today's scheduled automation (#575) — the crons + staff agents whose schedule fires today, each
 * with its last-run status and a run-now/trigger control (the shared `bn run <job>` path). Two
 * sibling cards so driving today's automated work lives alongside the goals digest. Each list is
 * empty-tolerant (a quiet day, or a weekend with no weekday crons) and errors are surfaced softly. */
function TodayAutomation() {
  const crons = useQuery(todayCronsQuery());
  const agents = useQuery(todayAgentsQuery());
  const now = Math.floor(Date.now() / 1000);

  const cronItems = crons.data?.agents ?? [];
  const agentItems = agents.data?.agents ?? [];

  // Stacked, not side-by-side. These used to be two `md:grid-cols-2` cards, which was fine on a
  // full-page measure and wrong the moment they moved into a column — the inner grid fought the
  // outer one and split the centre column into two thin lanes of its own.
  return (
    <>
      <Section label="Today's crons" meta={String(cronItems.length)}>
        {crons.isLoading ? (
          <Skeleton className="h-16 w-full" />
        ) : crons.isError ? (
          <p className="text-sm text-urgent">Could not load today&apos;s crons.</p>
        ) : cronItems.length === 0 ? (
          <p className="text-sm text-muted-foreground">No crons scheduled to fire today.</p>
        ) : (
          <ul className="space-y-1.5">
            {cronItems.map((item) => (
              <li key={item.job}>
                <AutomationRow item={item} now={now} label="run" />
              </li>
            ))}
          </ul>
        )}
      </Section>

      <PaneRule />

      <Section label="Today's agents" meta={String(agentItems.length)}>
        {agents.isLoading ? (
          <Skeleton className="h-16 w-full" />
        ) : agents.isError ? (
          <p className="text-sm text-urgent">Could not load today&apos;s agents.</p>
        ) : agentItems.length === 0 ? (
          <p className="text-sm text-muted-foreground">No agents scheduled to run today.</p>
        ) : (
          <ul className="space-y-1.5">
            {agentItems.map((item) => (
              <li key={item.job}>
                <AutomationRow item={item} now={now} label="trigger" />
              </li>
            ))}
          </ul>
        )}
      </Section>
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
    onError: (e) => pushToast(`Could not start Today's agent: ${(e as Error).message}`, "error"),
  });
  const stop = useMutation({
    mutationFn: () => stopTodayAgent(),
    onSuccess: () => {
      pushToast("Today's agent stopped", "success");
      invalidate();
    },
    onError: (e) => pushToast(`Could not stop Today's agent: ${(e as Error).message}`, "error"),
  });

  const busy = start.isPending || stop.isPending;

  return (
    <Pane
      scroll={false}
      className="flex-none"
      title={
        <span className="inline-flex items-center gap-1.5">
          <Bot className="h-3.5 w-3.5 text-primary" aria-hidden="true" />
          Today&apos;s agent
        </span>
      }
      meta={
        <span className="inline-flex items-center gap-1.5">
          <Badge variant={running ? "ok" : "muted"}>{running ? "running" : "not running"}</Badge>
          {running && view?.health && <Badge variant="info">{view.health}</Badge>}
        </span>
      }
      actions={
        running ? (
          <Button
            variant="secondary"
            size="sm"
            disabled={busy}
            title="Close the agent's tmux window and release its lease"
            onClick={() => {
              if (window.confirm("Stop Today's agent?")) stop.mutate();
            }}
          >
            {stop.isPending ? (
              <>
                <Loader2 className="h-4 w-4 animate-spin" /> stopping…
              </>
            ) : (
              <>
                <X className="h-4 w-4" /> Stop
              </>
            )}
          </Button>
        ) : (
          <Button
            size="sm"
            disabled={busy}
            title="Launch Today's agent into the tmux `planning` session, seeded with today's findings"
            onClick={() => start.mutate()}
          >
            {start.isPending ? (
              <>
                <Loader2 className="h-4 w-4 animate-spin" /> starting…
              </>
            ) : (
              <>
                <Sparkles className="h-4 w-4" /> Start agent
              </>
            )}
          </Button>
        )
      }
    >
      <div className="space-y-3 px-3.5 py-3">
      {agent.isLoading ? (
        <Skeleton className="h-16 w-full" />
      ) : !record ? (
        <p className="text-sm text-muted-foreground">
          Today's agent hasn't run yet. Starting it opens a live session in the{" "}
          <code className="rounded bg-muted px-1 py-0.5 font-mono text-xs">planning</code> tmux
          session, seeded with what this morning's crons found — then you steer it from here.
        </p>
      ) : (
        <>
          <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-muted-foreground">
            <span className="font-mono" title="steering target — `bn agent send <target>`">
              {record.target}
            </span>
            <span className="font-mono" title="tmux session:window — prefix C-p jumps here">
              {record.tmux_target}
            </span>
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
        </>
      )}
      </div>
    </Pane>
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
    onError: (e) => pushToast(`Today's agent failed: ${(e as Error).message}`, "error"),
  });

  const reviewMut = useMutation({
    mutationFn: ({ id, status }: { id: string; status: "accepted" | "dismissed" }) =>
      setSuggestionStatus(id, status),
    onSuccess: (r) => {
      pushToast(`Suggestion ${r.status}`, "success");
      invalidate();
    },
    onError: (e) => pushToast(`Review failed: ${(e as Error).message}`, "error"),
  });

  return (
    <Pane
      scroll={false}
      className="flex-none"
      title="Suggested tasks"
      meta={open.length > 0 ? String(open.length) : undefined}
      actions={
        /* The offline fold, demoted: Today's agent is the hosted session, and this is the
           deterministic re-synthesis from the findings store — useful to refresh the list without
           spending a turn, and what a headless box falls back to. */
        <Button
          variant="ghost"
          size="sm"
          disabled={runMut.isPending}
          title="Re-synthesize suggestions from the cron findings (offline, no agent turn)"
          onClick={() => runMut.mutate()}
        >
          {runMut.isPending ? (
            <>
              <Loader2 className="h-4 w-4 animate-spin" /> syncing…
            </>
          ) : (
            <>
              <RotateCw className="h-4 w-4" /> Re-synthesize
            </>
          )}
        </Button>
      }
    >
      {suggestions.isLoading ? (
        <Skeleton className="m-3.5 h-16" />
      ) : open.length === 0 ? (
        <p className="px-3.5 py-3 text-sm text-muted-foreground">
          No suggestions yet — start Today&apos;s agent, or hit{" "}
          <span className="font-medium">Re-synthesize</span> to fold the cron findings in offline.
        </p>
      ) : (
        // Rows divided by rules rather than gaps, and each row is its own hit area — the list is
        // the pane's whole body, so it scrolls independently of the columns beside it.
        <ul className="divide-y divide-border">
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
    </Pane>
  );
}

/**
 * `/` — the Today view (#490, Epic #488 Mission Control): the dashboard's default landing. A
 * personal *aimed vs achieved* digest — today's daily goals (with their quarter alignment) and the
 * quarter's goals on the left; what actually shipped (PRs merged, commits) on the right; then
 * what's still open / rolling into tomorrow. A tight top-of-day summary (goals done · PRs merged ·
 * agents · spend) leads. Read-only and mobile-first: `/api/goals` + `/api/wrap` (+ today's cost).
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
  const achievedNothing =
    doneGoals.length === 0 && mergedPrs.length === 0 && commits.length === 0;

  return (
    <div className="flex h-full min-h-0 flex-col">
      {/* The day's identity on one baseline with its date, flush to the top rule — the console's
          header treatment, rather than a document title floating above a stack of cards. */}
      <header className="flex flex-none items-baseline gap-3 border-b border-border px-4 py-2.5">
        <h1 className="font-display text-lg font-semibold tracking-tight text-foreground">Today</h1>
        <span className="font-mono text-[11px] tabular-nums text-muted-foreground/70">
          {todayDate ?? "—"} · aimed vs achieved
        </span>
      </header>

      {(goals.isError || wrap.isError) && (
        <div className="m-4 rounded-lg border border-urgent/30 bg-urgent-muted p-4 text-sm text-urgent">
          Could not load today&apos;s data.
        </div>
      )}

      {/* Hold the whole aimed-vs-achieved digest until every feeding query resolves — otherwise the
          summary tiles and "Achieved today" count render 0/empty on a cold load, then snap to the
          real values (0 PRs → 11 PRs, etc). OR across the queries, never AND (#544). */}
      {loading ? (
        <div className="grid min-h-0 flex-1 grid-cols-1 lg:grid-cols-[20rem_minmax(0,1fr)]" aria-hidden="true">
          {[0, 1].map((i) => (
            <div key={i} className="space-y-3 border-border p-3.5 lg:border-r">
              <Skeleton className="h-4 w-1/3" />
              <Skeleton className="h-4 w-2/3" />
              <Skeleton className="h-4 w-1/2" />
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
            "lg:grid-cols-[20rem_minmax(0,1fr)] lg:grid-rows-[minmax(0,1fr)_auto]",
            "wide:grid-cols-[20rem_minmax(0,1fr)_22rem] wide:grid-rows-1",
          )}
        >
          <aside
            aria-label="the day"
            className="flex min-h-0 flex-col overflow-y-auto border-border lg:col-start-1 lg:row-span-2 lg:row-start-1 lg:border-r wide:row-span-1"
          >
            <Section label="Aimed today" meta={openGoals.length > 0 ? `${openGoals.length} open` : undefined}>
              {dailyGoals.length === 0 ? (
                <p className="text-sm text-muted-foreground">
                  No daily goals yet — set them with{" "}
                  <code className="rounded bg-muted px-1 py-0.5 font-mono text-xs">bn daily add</code>.
                </p>
              ) : openGoals.length === 0 ? (
                <p className="text-sm text-muted-foreground">All of today&apos;s goals are done. 🎉</p>
              ) : (
                <ul className="space-y-2">
                  {openGoals.map((g) => (
                    <GoalRow key={g.id} goal={g} />
                  ))}
                </ul>
              )}
            </Section>

            {quarter && quarter.goals.length > 0 && (
              <>
                <PaneRule />
                <Section
                  label={`Quarter ${quarter.quarter}`}
                  meta={`${quarter.on_track}/${quarter.total} on track`}
                >
                  <ul className="space-y-2">
                    {quarter.goals.map((g) => (
                      <QuarterRow key={g.key} goal={g} />
                    ))}
                  </ul>
                </Section>
              </>
            )}

            <PaneRule />
            <Section
              label="Achieved today"
              meta={
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
                  <p className="text-sm text-muted-foreground">
                    Nothing wrapped up yet — close a goal or merge a PR to fill this in.
                  </p>
                )}
              </div>
            </Section>
          </aside>

          <section
            aria-label="proposals and open work"
            className="flex min-h-0 flex-col overflow-y-auto border-border lg:col-start-2 lg:row-start-1 lg:border-r"
          >
            <SuggestedTasks />
            <PaneRule />
            <TodayAutomation />
            {openTodos.length > 0 && (
              <>
                <PaneRule />
                <Section label="Open todos" meta="across branch notes · rolling forward">
                  <ul className="space-y-2">
                    {openTodos.slice(0, 12).map((t, i) => (
                      <li key={`${t.repo}/${t.branch}/${i}`} className="flex items-start gap-2 text-sm">
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
                </Section>
              </>
            )}
          </section>

          <aside
            aria-label="today's agent"
            className="flex min-h-0 flex-col overflow-y-auto border-t border-border lg:col-start-2 lg:row-start-2 wide:col-start-3 wide:row-start-1 wide:border-t-0"
          >
            <TodayAgentCard />
            <PaneRule />
            {/* The day's numbers sit with the agent that moved them, not in a banner overhead. */}
            <StatGrid cols={2} className="border-b border-border">
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
              <StatTile
                label="agents today"
                value={String(agentsToday)}
                tone={agentsToday > 0 ? "blue" : "grey"}
                title={
                  liveAgents > 0
                    ? `${finishedToday} finished + ${liveAgents} running now`
                    : "finished, ingested runs today"
                }
              />
              <StatTile
                label={spentIsAic ? "AIC spent" : "$ spent"}
                value={spentIsAic ? fmtAic(aicToday) : fmtUsd(usdToday)}
                tone="grey"
                title={
                  spentIsAic
                    ? "AI Credits — the metered Copilot bill for today's finished runs (lands once a run is ingested)"
                    : "No Copilot runs today, so there is no AIC bill — this is the cache-aware API-equivalent benchmark for today's finished runs. A benchmark, not a bill."
                }
              />
            </StatGrid>
          </aside>
        </div>
      )}
    </div>
  );
}
