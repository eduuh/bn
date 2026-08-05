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
  todayAgentsQuery,
  todayCronsQuery,
  wrapQuery,
} from "../api/client";
import { runCronJob, runTodayAgent, setSuggestionStatus } from "../api/writes";
import type { CatalogItem, DailyGoal, QuarterGoal, Suggestion } from "../api/types";
import { fmtAic, timeAgo } from "../lib/format";
import { pushToast } from "../lib/toast";
import { StatTile } from "../components/StatTile";
import { Badge, type BadgeProps } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import { Card } from "../components/ui/card";
import { Skeleton } from "../components/ui/skeleton";
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

/** A section header — title + a live count on the right. */
function SectionHead({ title, count }: { title: string; count?: number | string }) {
  return (
    <div className="flex items-center gap-2">
      <h3 className="text-sm font-semibold uppercase tracking-wide text-muted-foreground">
        {title}
      </h3>
      {count != null && <span className="text-xs text-muted-foreground">{count}</span>}
    </div>
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
    <li className="flex items-start gap-2 text-sm">
      <Sparkles className="mt-0.5 h-4 w-4 shrink-0 text-primary" aria-hidden="true" />
      <div className="min-w-0 flex-1 space-y-0.5">
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
    <Card className="relative overflow-hidden p-3 pl-4">
      <span className={cn("absolute inset-y-0 left-0 w-1", bar)} aria-hidden="true" />
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
    </Card>
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

  return (
    <div className="grid grid-cols-1 gap-4 md:grid-cols-2">
      <Card className="space-y-3 p-4">
        <SectionHead title="Today's crons" count={cronItems.length} />
        {crons.isLoading ? (
          <Skeleton className="h-16 w-full" />
        ) : crons.isError ? (
          <p className="text-sm text-urgent">Could not load today's crons.</p>
        ) : cronItems.length === 0 ? (
          <p className="text-sm text-muted-foreground">No crons scheduled to fire today.</p>
        ) : (
          <ul className="space-y-2">
            {cronItems.map((item) => (
              <li key={item.job}>
                <AutomationRow item={item} now={now} label="run" />
              </li>
            ))}
          </ul>
        )}
      </Card>

      <Card className="space-y-3 p-4">
        <SectionHead title="Today's agents" count={agentItems.length} />
        {agents.isLoading ? (
          <Skeleton className="h-16 w-full" />
        ) : agents.isError ? (
          <p className="text-sm text-urgent">Could not load today's agents.</p>
        ) : agentItems.length === 0 ? (
          <p className="text-sm text-muted-foreground">No agents scheduled to run today.</p>
        ) : (
          <ul className="space-y-2">
            {agentItems.map((item) => (
              <li key={item.job}>
                <AutomationRow item={item} now={now} label="trigger" />
              </li>
            ))}
          </ul>
        )}
      </Card>
    </div>
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
    <Card className="space-y-3 p-4">
      <div className="flex items-center gap-2">
        <SectionHead title="Suggested tasks" count={open.length} />
        <span className="flex-1" />
        <Button
          variant="secondary"
          size="sm"
          disabled={runMut.isPending}
          title="Run Today's agent — synthesize suggestions from the cron findings"
          onClick={() => runMut.mutate()}
        >
          {runMut.isPending ? (
            <>
              <Loader2 className="h-4 w-4 animate-spin" /> running…
            </>
          ) : (
            <>
              <Sparkles className="h-4 w-4" /> Run
            </>
          )}
        </Button>
      </div>

      {suggestions.isLoading ? (
        <Skeleton className="h-16 w-full" />
      ) : open.length === 0 ? (
        <p className="text-sm text-muted-foreground">
          No suggestions yet — hit <span className="font-medium">Run</span> to synthesize them from
          the cron findings.
        </p>
      ) : (
        <ul className="space-y-3">
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
    </Card>
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

  const loading = goals.isLoading || wrap.isLoading || cost.isLoading || fleet.isLoading;
  const achievedNothing =
    doneGoals.length === 0 && mergedPrs.length === 0 && commits.length === 0;

  return (
    <div className="mx-auto max-w-3xl space-y-5 pb-8">
      <header className="space-y-1">
        <h2 className="text-2xl font-semibold tracking-tight">Today</h2>
        <p className="text-sm text-muted-foreground">{todayDate ?? "—"} · aimed vs achieved</p>
      </header>

      {loading && (
        <div className="space-y-5" aria-hidden="true">
          <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
            {[0, 1, 2, 3].map((i) => (
              <Skeleton key={i} className="h-20 w-full" />
            ))}
          </div>
          <div className="grid grid-cols-1 gap-4 md:grid-cols-2">
            {[0, 1].map((i) => (
              <Card key={i} className="space-y-3 p-4">
                <Skeleton className="h-4 w-1/3" />
                <Skeleton className="h-4 w-2/3" />
                <Skeleton className="h-4 w-1/2" />
              </Card>
            ))}
          </div>
        </div>
      )}

      {/* Hold the whole aimed-vs-achieved digest until every feeding query resolves — otherwise the
          summary tiles and "Achieved today" count render 0/empty on a cold load, then snap to the
          real values (0 PRs → 11 PRs, etc). OR across the queries, never AND (#544). */}
      {!loading && (
        <>
          <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
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
              label="AIC spent"
              value={fmtAic(aicToday)}
              tone="grey"
              title="AI Credits — the metered Copilot bill for today's finished runs (lands once a run is ingested)"
            />
          </div>

          <div className="grid grid-cols-1 gap-4 md:grid-cols-2">
            <Card className="space-y-4 p-4">
              <SectionHead title="Aimed today" count={openGoals.length} />
              {dailyGoals.length === 0 ? (
                <p className="text-sm text-muted-foreground">
                  No daily goals yet — set them with{" "}
                  <code className="rounded bg-muted px-1 py-0.5 font-mono text-xs">
                    bn daily add
                  </code>
                  .
                </p>
              ) : openGoals.length === 0 ? (
                <p className="text-sm text-muted-foreground">All of today's goals are done. 🎉</p>
              ) : (
                <ul className="space-y-2">
                  {openGoals.map((g) => (
                    <GoalRow key={g.id} goal={g} />
                  ))}
                </ul>
              )}

              {quarter && quarter.goals.length > 0 && (
                <div className="space-y-2 border-t border-border pt-3">
                  <div className="flex items-center justify-between gap-2">
                    <h4 className="text-sm font-semibold text-foreground">
                      Quarter {quarter.quarter}
                    </h4>
                    <span className="text-xs text-muted-foreground">
                      {quarter.on_track}/{quarter.total} on track
                    </span>
                  </div>
                  <ul className="space-y-2">
                    {quarter.goals.map((g) => (
                      <QuarterRow key={g.key} goal={g} />
                    ))}
                  </ul>
                </div>
              )}
            </Card>

            <Card className="space-y-4 p-4">
              <SectionHead title="Achieved today" count={doneGoals.length + mergedPrs.length} />

              {doneGoals.length > 0 && (
                <div className="space-y-2">
                  <div className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
                    Goals completed
                  </div>
                  <ul className="space-y-2">
                    {doneGoals.map((g) => (
                      <GoalRow key={g.id} goal={g} />
                    ))}
                  </ul>
                </div>
              )}

              {mergedPrs.length > 0 && (
                <div className="space-y-2">
                  <div className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
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
                <div className="space-y-2">
                  <div className="flex items-baseline gap-2 text-xs font-medium uppercase tracking-wide text-muted-foreground">
                    Commits
                    <span className="font-mono normal-case text-muted-foreground/80">
                      → bn sync
                    </span>
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
            </Card>
          </div>

          <SuggestedTasks />

          <TodayAutomation />

          {openTodos.length > 0 && (
            <Card className="space-y-3 p-4">
              <SectionHead title="Open todos" count="across branch notes · rolling forward" />
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
            </Card>
          )}
        </>
      )}

      {(goals.isError || wrap.isError) && (
        <Card className="border-urgent/30 bg-urgent-muted p-4 text-sm text-urgent">
          Could not load today's data.
        </Card>
      )}
    </div>
  );
}
