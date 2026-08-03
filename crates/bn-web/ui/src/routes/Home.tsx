import { useMemo, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { AlertTriangle, ArrowUpRight, Ban, Check, Play, Sparkles } from "lucide-react";

import {
  attnQuery,
  doneTasksQuery,
  issuesQuery,
  pipelineStagesQuery,
  queryKeys,
  tasksQuery,
} from "../api/client";
import type {
  AttnItem,
  PipelineStageWork,
  RepoIssues,
  StageTask,
  TaskRow,
} from "../api/types";
import { createTask, sendMessage, taskAction, taskAgent } from "../api/writes";
import { PipelineFlow } from "../components/PipelineFlow";
import { NewTaskDialog } from "../components/NewTaskDialog";
import { SteerButton } from "../components/SteerButton";
import { ToastStack } from "../components/ToastStack";
import { pushToast } from "../lib/toast";
import { Badge } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import { Card } from "../components/ui/card";
import { cn } from "../lib/utils";

/**
 * Has this task earned the right to be started?
 *
 * **Mirrors `actions::is_triaged` on the server, and is not the enforcement.** The endpoint answers
 * 409 `not_triaged` regardless of what this returns — the CLI and every agent POST to it too. This
 * exists so the operator is told *why* before clicking, not to decide whether it is allowed.
 *
 * Both halves are required, because two different agents contribute one each: `triage` picks what
 * is doable today and stamps `day:`, `improve` makes the task SMART and records `smart:`. A future
 * `day:` is deliberately not startable — it was triaged for another day.
 */
function isTriaged(task: TaskRow, today: string): boolean {
  return !!task.day && task.day <= today && task.smart === "ok";
}

/** Why a task cannot be started yet, phrased as the thing to do about it. */
function blockedReason(task: TaskRow, today: string): string {
  if (task.smart === "stuck") return "the improve agent gave up — this one needs you";
  if (task.smart !== "ok") return "has not passed the SMART gate — run Improve";
  if (!task.day) return "not scheduled — run Triage for today";
  if (task.day > today) return `triaged for ${task.day}, not today`;
  return "not triaged yet";
}

/** The shared leading cells of a task row: id, priority, summary, repo. */
function RowHead({ task, today }: { task: TaskRow; today: string }) {
  const overdue = !!task.day && task.day < today;
  return (
    <>
      <span className="font-mono text-[11px] text-muted-foreground/80">{task.id}</span>
      {task.priority && (
        <span className="font-mono text-[11px] font-semibold text-foreground">
          ({task.priority})
        </span>
      )}
      <span className="min-w-0 flex-1 text-sm leading-snug text-foreground">{task.summary}</span>
      {task.repo && (
        <span className="font-mono text-[11px] text-muted-foreground">[{task.repo}]</span>
      )}
      {task.day && (
        <span
          className={cn(
            "font-mono text-[11px]",
            overdue ? "font-semibold text-warn" : "text-muted-foreground",
          )}
          title={overdue ? "carried over from an earlier day" : undefined}
        >
          {task.day}
          {overdue ? "!" : ""}
        </span>
      )}
    </>
  );
}

const ROW = "flex flex-wrap items-center gap-x-2.5 gap-y-1 border-b border-border/60 py-1.5 last:border-b-0";

/** A task that has been triaged: the only kind that offers Start. */
function ReadyRow({ task, today }: { task: TaskRow; today: string }) {
  const qc = useQueryClient();
  const invalidate = () => {
    void qc.invalidateQueries({ queryKey: queryKeys.tasks("today") });
    void qc.invalidateQueries({ queryKey: queryKeys.pipelineStages });
  };
  const start = useMutation({
    mutationFn: () => taskAction(task.id, "wip"),
    onSuccess: () => {
      pushToast(`Started ${task.id}`, "success");
      invalidate();
    },
    // The gate lives on the server, so a refusal can still arrive here — say so plainly rather
    // than letting the row look broken.
    onError: (e) => pushToast(`Could not start ${task.id}: ${(e as Error).message}`, "error"),
  });

  return (
    <li className={ROW}>
      <RowHead task={task} today={today} />
      {task.est && <span className="font-mono text-[11px] text-muted-foreground">{task.est}</span>}
      <Badge variant="ok" title="passed the SMART gate and triaged for today">
        ready
      </Badge>
      <Button
        size="sm"
        disabled={start.isPending}
        onClick={() => start.mutate()}
        title="start this task — marks it wip"
      >
        <Play className="h-4 w-4" /> Start
      </Button>
    </li>
  );
}

/** A task that has *not* been triaged. Start is present but disabled: an absent button would read
 * as "this task cannot be started ever", when the truth is "not yet, and here is what to do". */
function UntriagedRow({ task, today }: { task: TaskRow; today: string }) {
  const reason = blockedReason(task, today);
  const improve = useMutation({
    mutationFn: (steer: string) => taskAgent(task.id, "improve", steer || undefined),
    onSuccess: () => pushToast(`Improve agent started on ${task.id} — watch the fleet`, "success"),
    onError: (e) => pushToast(`Could not start improve: ${(e as Error).message}`, "error"),
  });

  return (
    <li className={ROW}>
      <RowHead task={task} today={today} />
      <Badge
        variant={task.smart === "stuck" ? "urgent" : task.smart === "fail" ? "warn" : "muted"}
        title={reason}
      >
        {task.smart === "stuck" ? "needs you" : task.smart === "fail" ? "not ready" : "unscoped"}
      </Badge>
      <SteerButton
        label="Improve"
        title="run the improve agent on this task"
        placeholder="things to look out for (optional)"
        busy={improve.isPending}
        onSubmit={(steer) => improve.mutate(steer)}
      />
      <Button size="sm" variant="secondary" disabled title={reason} className="opacity-50">
        <Play className="h-4 w-4" /> Start
      </Button>
    </li>
  );
}

/** Work in flight: its stage, whatever agent is on it, and the two things you can do to it. */
function InFlightRow({ task, today }: { task: StageTask; today: string }) {
  const qc = useQueryClient();
  const invalidate = () => {
    void qc.invalidateQueries({ queryKey: queryKeys.tasks("today") });
    void qc.invalidateQueries({ queryKey: queryKeys.pipelineStages });
    void qc.invalidateQueries({ queryKey: queryKeys.doneTasks("today") });
    void qc.invalidateQueries({ queryKey: queryKeys.fleet });
  };
  const agent = task.agent;

  const finish = useMutation({
    mutationFn: () => taskAction(task.id, "done"),
    onSuccess: () => {
      pushToast(`Done ${task.id}`, "success");
      invalidate();
    },
    onError: (e) => pushToast(`Could not finish ${task.id}: ${(e as Error).message}`, "error"),
  });
  const cancel = useMutation({
    mutationFn: (reason: string) => taskAction(task.id, "cancel", reason || undefined),
    onSuccess: () => {
      pushToast(`Cancelled ${task.id}`, "success");
      invalidate();
    },
    onError: (e) => pushToast(`Could not cancel ${task.id}: ${(e as Error).message}`, "error"),
  });
  // Addressed by `target` (`repo:branch`) — the identity every bn surface steers an agent by, and
  // the same key this task was joined to it on.
  const steer = useMutation({
    mutationFn: (msg: string) => sendMessage(agent?.target ?? "", msg),
    onSuccess: () => pushToast(`Steered ${agent?.target}`, "success"),
    onError: (e) => pushToast(`Could not steer: ${(e as Error).message}`, "error"),
  });

  return (
    <li className={ROW}>
      <RowHead task={task} today={today} />
      {task.branch && (
        <span className="font-mono text-[11px] text-muted-foreground/80">{task.branch}</span>
      )}
      {task.stage && (
        <Badge variant="info" className="font-mono">
          {task.stage}
        </Badge>
      )}
      {/* What this worktree is *actually* running (#1041), now carried by the same projection that
          joined the task — an explicit `--skill debug` sits in `execute` while running `debug`, and
          showing the stage default there would be a confident lie. */}
      {task.skill && (
        <Badge
          variant="muted"
          className="font-mono"
          title={`running the \`${task.skill}\` workflow`}
        >
          ⌁ {task.skill}
        </Badge>
      )}
      {/* The agent is only shown when one is really joined to this task — an always-present chip
          would make an abandoned worktree look staffed. */}
      {agent && (
        <a
          href={`/agents/${encodeURIComponent(agent.session_id)}`}
          title={`${agent.harness} · ${agent.health} · idle ${Math.round(agent.idle_secs / 60)}m — open in the fleet`}
          className={cn(
            "inline-flex items-center gap-1 rounded-md border px-1.5 py-0.5 font-mono text-[11px]",
            agent.health === "stuck"
              ? "border-urgent/50 bg-urgent/10 text-urgent"
              : agent.health === "idle"
                ? "border-warn/50 bg-warn/10 text-warn"
                : "border-ok/50 bg-ok/10 text-ok",
          )}
        >
          ◉ {agent.harness} · {agent.health}
        </a>
      )}
      {/* Steering needs something to steer. Without a joined agent the control is disabled and
          says why, rather than accepting a message that would go nowhere. */}
      {agent ? (
        <SteerButton
          label="Steer"
          title={`send a message to ${agent.target}`}
          placeholder="what should it do differently?"
          busy={steer.isPending}
          onSubmit={(msg) => msg && steer.mutate(msg)}
        />
      ) : (
        <Button
          size="sm"
          variant="secondary"
          disabled
          title="no agent is running on this task"
          className="opacity-50"
        >
          Steer
        </Button>
      )}
      <Button
        size="sm"
        variant="secondary"
        disabled={finish.isPending}
        onClick={() => finish.mutate()}
        title="mark done"
      >
        <Check className="h-4 w-4" />
      </Button>
      <Button
        size="sm"
        variant="ghost"
        disabled={cancel.isPending}
        title="cancel this task"
        onClick={() => {
          // Naming the agent matters, but only say what actually happens: cancelling the task does
          // *not* yet stop the agent — the stop-vs-pause semantics are still undecided, so the
          // agent keeps running until it is dealt with in the fleet. Claiming otherwise would be
          // worse than saying nothing, because the operator would walk away from live work.
          const warn = agent
            ? `\n\n${agent.target} is still running on it and will NOT be stopped — stop it in the fleet.`
            : "";
          const reason = window.prompt(`Cancel ${task.id}?${warn}\n\nReason (optional):`);
          if (reason === null) return;
          cancel.mutate(reason);
        }}
      >
        <Ban className="h-4 w-4 text-urgent" />
      </Button>
    </li>
  );
}

/** One attention card, compact, with the steerable investigate button. */
function AttnRow({ item }: { item: AttnItem }) {
  const investigate = useMutation({
    // Investigation is a task agent too: it improves the record before anything executes.
    mutationFn: (steer: string) => taskAgent(item.id, "improve", steer || undefined),
    onSuccess: () => pushToast(`Investigating ${item.id} — watch the fleet`, "success"),
    onError: (e) => pushToast(`Could not start: ${(e as Error).message}`, "error"),
  });
  const tone =
    item.severity === "urgent" ? "urgent" : item.severity === "warn" ? "warn" : "info";
  return (
    <li className={ROW}>
      <Badge variant={tone} className="uppercase tracking-wide">
        {item.severity}
      </Badge>
      <span className="min-w-0 flex-1 text-sm leading-snug text-foreground">{item.title}</span>
      {item.repo && (
        <span className="font-mono text-[11px] text-muted-foreground">
          {item.repo}
          {item.branch ? `/${item.branch}` : ""}
        </span>
      )}
      <SteerButton
        label="Investigate"
        title="run an investigation agent, steered"
        placeholder="things to look out for (optional)"
        busy={investigate.isPending}
        onSubmit={(steer) => investigate.mutate(steer)}
      />
    </li>
  );
}

/** One repo's issues, with its freshness stated rather than implied. */
function RepoIssueGroup({ repo }: { repo: RepoIssues }) {
  const age = repo.fetched_at
    ? Math.max(0, Math.round((Date.now() / 1000 - repo.fetched_at) / 60))
    : null;
  return (
    <div className="border-b border-border/60 py-2 last:border-b-0">
      <div className="flex flex-wrap items-baseline gap-2">
        <span className="font-mono text-[12px] font-semibold text-foreground">{repo.repo}</span>
        <span className="text-[11px] text-muted-foreground">{repo.issues.length} open</span>
        {/* The honesty contract from #999, surfaced: an empty list with an error is NOT the same
            fact as an empty backlog, so never let them render alike. */}
        {repo.error && (
          <span
            className="inline-flex items-center gap-1 text-[11px] text-warn"
            title={repo.error}
          >
            <AlertTriangle className="h-3 w-3" />
            {repo.stale
              ? `stale${age !== null ? `, fetched ${age}m ago` : ""}`
              : "could not fetch"}
          </span>
        )}
      </div>
      {repo.issues.length === 0 && !repo.error && (
        <p className="mt-0.5 text-[11px] text-muted-foreground">No open issues.</p>
      )}
      <ul className="mt-0.5">
        {repo.issues.slice(0, 8).map((issue) => (
          <li key={issue.url} className="flex items-center gap-2 py-0.5 text-[12px]">
            <span className="font-mono text-muted-foreground/80">#{issue.number}</span>
            <a
              href={issue.url}
              target="_blank"
              rel="noreferrer"
              className="min-w-0 flex-1 truncate text-foreground hover:text-primary"
            >
              {issue.title}
            </a>
            {issue.is_assigned && (
              <span className="text-[10.5px] text-muted-foreground">assigned</span>
            )}
          </li>
        ))}
      </ul>
    </div>
  );
}

function SectionCard({
  title,
  count,
  action,
  tone,
  children,
}: {
  title: string;
  count?: string;
  action?: React.ReactNode;
  tone?: "ready";
  children: React.ReactNode;
}) {
  return (
    <Card className={cn("p-3", tone === "ready" && "border-ok/40")}>
      <div className="mb-1 flex flex-wrap items-baseline gap-2">
        <h2 className="font-display text-sm font-semibold tracking-tight text-foreground">
          {title}
        </h2>
        {count && (
          <span className="font-mono text-[11px] tabular-nums text-muted-foreground/70">
            {count}
          </span>
        )}
        {action && <span className="ml-auto">{action}</span>}
      </div>
      {children}
    </Card>
  );
}

/**
 * `/` — the task-first home (#1006, EPIC #993), reorganised as **the task lifecycle** (#1033).
 *
 * The screen used to be one undifferentiated Tasks list under a pipeline drawing that could not be
 * clicked into: the drawing counted worktrees from the event log while the list filtered tasks on
 * `task.stage`, a field that is read in one place and written in none. Selecting a stage therefore
 * produced an empty list *always*, under a node claiming several were in flight.
 *
 * Now each list is one state — untriaged, ready, in flight, done — and each control is the one
 * legal transition out of it. Start appears only on triaged work, which the server enforces
 * independently (409 `not_triaged`), because the CLI and every agent hit the same endpoint and a
 * gate that only the dashboard honours is a suggestion.
 *
 * The in-flight rows and the pipeline counts come from a single projection joined on
 * `(repo, branch)` — the identity the stage board, the task store and the live fleet already
 * share — so a node's number and the rows beneath it cannot disagree.
 */
export function Home() {
  const tasks = useQuery(tasksQuery("today"));
  const stages = useQuery(pipelineStagesQuery());
  const done = useQuery(doneTasksQuery());
  const attn = useQuery(attnQuery());
  const issues = useQuery(issuesQuery());
  const qc = useQueryClient();

  const [stage, setStage] = useState<string | undefined>();
  const [creating, setCreating] = useState(false);

  const today = tasks.data?.today ?? "";
  const openTasks = useMemo(() => tasks.data?.tasks ?? [], [tasks.data]);

  // Partition the open store by lifecycle state. `wip` is in flight even when no worktree has
  // reached a stage yet, so status leads and the pipeline refines.
  const { ready, untriaged, wip } = useMemo(() => {
    const ready: TaskRow[] = [];
    const untriaged: TaskRow[] = [];
    const wip: TaskRow[] = [];
    for (const t of openTasks) {
      if (t.status === "wip") wip.push(t);
      else if (isTriaged(t, today)) ready.push(t);
      else untriaged.push(t);
    }
    return { ready, untriaged, wip };
  }, [openTasks, today]);

  const stageList: PipelineStageWork[] = stages.data?.stages ?? [];
  const selected = stage ? stageList.find((s) => s.id === stage) : undefined;

  // With a stage selected the rows come from the projection (so count and list are one value).
  // With none, fall back to every wip task — including work whose worktree never recorded a stage,
  // which would otherwise vanish from the screen entirely.
  const stagedIds = new Set(stageList.flatMap((s) => s.tasks.map((t) => t.id)));
  const inFlightRows: StageTask[] = selected
    ? selected.tasks
    : [
        ...stageList.flatMap((s) => s.tasks),
        ...wip.filter((t) => !stagedIds.has(t.id)),
      ];

  // #1041 rebuilt this `repo/branch → skill` map in the browser from a second endpoint. It is gone
  // because `/api/pipeline/stages` already joins on exactly that key server-side and hands each row
  // its own `skill` — the same fact, without a client-side join to keep in step.

  const create = useMutation({
    mutationFn: createTask,
    onSuccess: (r) => {
      pushToast(`Created ${r.id}`, "success");
      setCreating(false);
      void qc.invalidateQueries({ queryKey: queryKeys.tasks("today") });
    },
    onError: (e) => pushToast(`Could not create: ${(e as Error).message}`, "error"),
  });
  const triage = useMutation({
    mutationFn: () => taskAgent("t0", "triage"),
    onSuccess: () => pushToast("Triage started — watch the fleet", "success"),
    onError: (e) => pushToast(`Could not start triage: ${(e as Error).message}`, "error"),
  });

  const triageButton = (
    <Button
      size="sm"
      variant="secondary"
      disabled={triage.isPending}
      onClick={() => triage.mutate()}
      title="rank everything and pick what's doable today"
    >
      <Play className="h-4 w-4" /> Triage for today
    </Button>
  );

  // Severity first, then most recent — the same order `/attention` uses. Sorting matters more than
  // it looks: this list is capped, and unsorted it could push an urgent item off the bottom.
  const attnItems = useMemo(() => {
    const rank = (s: string) => (s === "urgent" ? 0 : s === "warn" ? 1 : 2);
    return [...(attn.data?.items ?? [])].sort(
      (a, b) => rank(a.severity) - rank(b.severity) || b.created - a.created,
    );
  }, [attn.data]);

  const repos = issues.data?.repos ?? [];
  const degraded = (issues.data?.stale_repos ?? 0) + (issues.data?.unreachable_repos ?? 0);
  const doneRows = done.data?.tasks ?? [];

  return (
    <div className="space-y-4 pb-8">
      <div className="flex flex-wrap items-baseline gap-3">
        <h1 className="font-display text-lg font-semibold tracking-tight text-foreground">Today</h1>
        <span className="font-mono text-[11px] tabular-nums text-muted-foreground/70">{today}</span>
        <span className="ml-auto flex items-center gap-2">
          {triageButton}
          <Button size="sm" onClick={() => setCreating(true)}>
            <Sparkles className="h-4 w-4" /> New task
          </Button>
        </span>
      </div>

      <SectionCard
        title="Ready to start"
        tone="ready"
        count={
          tasks.isLoading ? "loading…" : `${ready.length} triaged · the only tasks that can be started`
        }
      >
        {ready.length === 0 ? (
          <p className="py-2 text-sm text-muted-foreground">
            Nothing is ready — run <span className="font-medium">Triage for today</span> to pick
            what's doable, or <span className="font-medium">Improve</span> a task past the SMART
            gate.
          </p>
        ) : (
          <ul>
            {ready.map((t) => (
              <ReadyRow key={t.id} task={t} today={today} />
            ))}
          </ul>
        )}
      </SectionCard>

      <SectionCard
        title={selected ? `In flight · ${selected.id}` : "In flight"}
        count={
          stages.isLoading
            ? "loading…"
            : `${stages.data?.in_flight ?? 0} across the pipeline${
                selected ? ` · ${selected.count} here` : ""
              }`
        }
        action={
          stage && (
            <button
              type="button"
              onClick={() => setStage(undefined)}
              className="text-[11px] text-primary hover:underline"
            >
              clear filter
            </button>
          )
        }
      >
        {stageList.length > 0 && (
          <div className="mb-2">
            <PipelineFlow stages={stageList} selected={stage} onSelect={setStage} bare />
          </div>
        )}

        {inFlightRows.length === 0 ? (
          <p className="py-2 text-sm text-muted-foreground">
            {selected ? `Nothing is in ${selected.id}.` : "Nothing is in flight."}
          </p>
        ) : (
          <ul>
            {inFlightRows.map((t) => (
              <InFlightRow key={t.id} task={t} today={today} />
            ))}
          </ul>
        )}

        {/* A worktree at a stage with no task behind it. Named rather than dropped: it is exactly
            the gap #993 closes, and hiding it would make the count read as wrong instead of as a
            fact about the pipeline. */}
        {selected?.orphans.map((o) => (
          <p key={`${o.repo}/${o.branch}`} className="mt-1.5 text-[11px] text-muted-foreground">
            ⌁ <span className="font-mono text-foreground">{o.repo}/{o.branch}</span> is in{" "}
            {selected.id} with no task attached.
          </p>
        ))}
      </SectionCard>

      <SectionCard
        title="Untriaged"
        count={tasks.isLoading ? "loading…" : `${untriaged.length} waiting · cannot be started`}
        action={triageButton}
      >
        {untriaged.length === 0 ? (
          <p className="py-2 text-sm text-muted-foreground">Everything open has been triaged.</p>
        ) : (
          <ul>
            {untriaged.map((t) => (
              <UntriagedRow key={t.id} task={t} today={today} />
            ))}
          </ul>
        )}
      </SectionCard>

      <SectionCard
        title="Done today"
        count={
          done.isLoading
            ? "loading…"
            : `${done.data?.done ?? 0} finished${
                (done.data?.cancelled ?? 0) > 0 ? ` · ${done.data?.cancelled} cancelled` : ""
              }`
        }
      >
        {doneRows.length === 0 ? (
          <p className="py-2 text-sm text-muted-foreground">Nothing finished yet today.</p>
        ) : (
          <ul>
            {doneRows.map((t) => {
              const cancelled = t.status === "cancelled";
              return (
                <li key={t.id} className={ROW}>
                  <Badge variant={cancelled ? "muted" : "ok"}>{cancelled ? "✕" : "✓"}</Badge>
                  <span className="font-mono text-[11px] text-muted-foreground/80">{t.id}</span>
                  <span
                    className={cn(
                      "min-w-0 flex-1 text-sm leading-snug",
                      cancelled ? "text-muted-foreground line-through" : "text-foreground",
                    )}
                  >
                    {t.summary}
                  </span>
                  {t.repo && (
                    <span className="font-mono text-[11px] text-muted-foreground">[{t.repo}]</span>
                  )}
                  {/* Cancelled work is shown but never counted as finished — the two are separate
                      figures in the header for the same reason. */}
                  {cancelled && t.reason && (
                    <span className="font-mono text-[11px] text-muted-foreground/80">
                      {t.reason}
                    </span>
                  )}
                </li>
              );
            })}
          </ul>
        )}
      </SectionCard>

      <SectionCard
        title="Needs you"
        count={attn.isLoading ? "loading…" : `${attnItems.length} open · most severe first`}
        action={
          <a
            href="/attention"
            className="inline-flex items-center gap-0.5 text-[11px] text-primary"
          >
            all {attnItems.length} <ArrowUpRight className="h-3 w-3" />
          </a>
        }
      >
        {attnItems.length === 0 ? (
          <p className="py-2 text-sm text-muted-foreground">Nothing is waiting on you.</p>
        ) : (
          <>
            <ul>
              {attnItems.slice(0, 8).map((i) => (
                <AttnRow key={i.id} item={i} />
              ))}
            </ul>
            {attnItems.length > 8 && (
              <p className="mt-1.5 text-[11px] text-muted-foreground">
                Showing the 8 most severe of {attnItems.length}.
              </p>
            )}
          </>
        )}
      </SectionCard>

      <SectionCard
        title="Issues · all repos"
        count={
          issues.isLoading
            ? "loading…"
            : `${issues.data?.count ?? 0} open${degraded > 0 ? ` · ${degraded} repo(s) degraded` : ""}`
        }
      >
        {repos.length === 0 ? (
          <p className="py-2 text-sm text-muted-foreground">
            {issues.isLoading ? "Loading…" : "No repos known — set [managed_repos].repos."}
          </p>
        ) : (
          <div>
            {repos.map((r) => (
              <RepoIssueGroup key={r.repo} repo={r} />
            ))}
          </div>
        )}
        <a
          href="/repos"
          className="mt-2 inline-flex items-center gap-0.5 text-[11px] text-primary"
        >
          all repos <ArrowUpRight className="h-3 w-3" />
        </a>
      </SectionCard>

      <NewTaskDialog
        open={creating}
        busy={create.isPending}
        onOpenChange={setCreating}
        onSubmit={(body) => create.mutate(body)}
      />
      <ToastStack />
    </div>
  );
}

export default Home;
