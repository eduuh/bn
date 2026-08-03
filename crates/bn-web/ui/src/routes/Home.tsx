import { useMemo, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { AlertTriangle, ArrowUpRight, Check, Play, Sparkles } from "lucide-react";

import { attnQuery, issuesQuery, pipelineQuery, queryKeys, tasksQuery } from "../api/client";
import type { AttnItem, RepoIssues, TaskRow } from "../api/types";
import { createTask, taskAction, taskAgent } from "../api/writes";
import { PipelineFlow } from "../components/PipelineFlow";
import { NewTaskDialog } from "../components/NewTaskDialog";
import { SteerButton } from "../components/SteerButton";
import { ToastStack } from "../components/ToastStack";
import { pushToast } from "../lib/toast";
import { Badge } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import { Card } from "../components/ui/card";
import { cn } from "../lib/utils";

/** How the SMART verdict reads on a row. `null` is a real state — never judged — not an absence. */
function smartBadge(smart: string | null) {
  switch (smart) {
    case "ok":
      return { label: "ready", variant: "ok" as const, hint: "passed the SMART gate" };
    case "stuck":
      return { label: "needs you", variant: "urgent" as const, hint: "the improve agent gave up" };
    case "fail":
      return { label: "not ready", variant: "warn" as const, hint: "failed the SMART gate" };
    default:
      return { label: "unscoped", variant: "muted" as const, hint: "never judged" };
  }
}

/** One task row: what it is, where, and the one action it actually admits. */
function TaskRowView({ task, today }: { task: TaskRow; today: string }) {
  const qc = useQueryClient();
  const smart = smartBadge(task.smart);
  const overdue = !!task.day && task.day < today;

  const invalidate = () => {
    void qc.invalidateQueries({ queryKey: queryKeys.tasks("today") });
    void qc.invalidateQueries({ queryKey: queryKeys.tasks("all") });
  };
  const done = useMutation({
    mutationFn: () => taskAction(task.id, "done"),
    onSuccess: () => {
      pushToast(`Done ${task.id}`, "success");
      invalidate();
    },
    onError: (e) => pushToast(`Could not finish ${task.id}: ${(e as Error).message}`, "error"),
  });
  const improve = useMutation({
    mutationFn: (steer: string) => taskAgent(task.id, "improve", steer || undefined),
    // The agent runs for minutes; the server answers as soon as it is spawned, so say that
    // rather than implying the work is finished.
    onSuccess: () => pushToast(`Improve agent started on ${task.id} — watch the fleet`, "success"),
    onError: (e) => pushToast(`Could not start improve: ${(e as Error).message}`, "error"),
  });

  return (
    <li className="flex flex-wrap items-center gap-x-2.5 gap-y-1 border-b border-border/60 py-1.5 last:border-b-0">
      <span className="font-mono text-[11px] text-muted-foreground/80">{task.id}</span>
      {task.priority && (
        <span className="font-mono text-[11px] font-semibold text-foreground">({task.priority})</span>
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
      {task.stage && (
        <Badge variant="info" className="font-mono">
          {task.stage}
        </Badge>
      )}
      <Badge variant={smart.variant} title={smart.hint}>
        {smart.label}
      </Badge>

      {task.smart !== "ok" && (
        <SteerButton
          label="Improve"
          title="run the improve agent on this task"
          placeholder="things to look out for (optional)"
          busy={improve.isPending}
          onSubmit={(steer) => improve.mutate(steer)}
        />
      )}
      <Button
        size="sm"
        variant="secondary"
        disabled={done.isPending}
        onClick={() => done.mutate()}
        title="mark done"
      >
        <Check className="h-4 w-4" />
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
    <li className="flex flex-wrap items-center gap-x-2.5 gap-y-1 border-b border-border/60 py-1.5 last:border-b-0">
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
  children,
}: {
  title: string;
  count?: string;
  action?: React.ReactNode;
  children: React.ReactNode;
}) {
  return (
    <Card className="p-3">
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
 * `/` — the task-first home (#1006, EPIC #993).
 *
 * The dashboard used to open on a console whose primary verb was **Start an agent**: a free-text
 * prompt box that spawned work leaving no durable record of what the work *was*. So work existed
 * only as a running agent, and when the agent died the intent died with it.
 *
 * Now the task is the unit. This screen is the three things that decide what happens next —
 * what's scheduled for today, what needs *you*, and the issue reservoir the list refills from —
 * over the pipeline those tasks flow through. There is no way to start an agent from here that
 * does not begin with a task; **New task** in the rail is the only origin.
 */
export function Home() {
  const tasks = useQuery(tasksQuery("today"));
  const attn = useQuery(attnQuery());
  const issues = useQuery(issuesQuery());
  const pipeline = useQuery(pipelineQuery());
  const qc = useQueryClient();

  const [stage, setStage] = useState<string | undefined>();
  const [creating, setCreating] = useState(false);

  const today = tasks.data?.today ?? "";
  const rows = useMemo(() => {
    const all = tasks.data?.tasks ?? [];
    return stage ? all.filter((t) => t.stage === stage) : all;
  }, [tasks.data, stage]);

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

  const attnItems = attn.data?.items ?? [];
  const repos = issues.data?.repos ?? [];
  const degraded = (issues.data?.stale_repos ?? 0) + (issues.data?.unreachable_repos ?? 0);

  return (
    <div className="space-y-4 pb-8">
      <div className="flex flex-wrap items-baseline gap-3">
        <h1 className="font-display text-lg font-semibold tracking-tight text-foreground">Today</h1>
        <span className="font-mono text-[11px] tabular-nums text-muted-foreground/70">{today}</span>
        <Button size="sm" className="ml-auto" onClick={() => setCreating(true)}>
          <Sparkles className="h-4 w-4" /> New task
        </Button>
      </div>

      {pipeline.data && (
        <PipelineFlow stages={pipeline.data.stages} selected={stage} onSelect={setStage} />
      )}

      <SectionCard
        title={stage ? `Tasks in ${stage}` : "Tasks"}
        count={tasks.isLoading ? "loading…" : `${rows.length} actionable`}
      >
        {rows.length === 0 ? (
          <p className="py-2 text-sm text-muted-foreground">
            {stage
              ? `Nothing is in ${stage}.`
              : "Nothing scheduled for today — create a task, or pull one from the issues below."}
          </p>
        ) : (
          <ul>
            {rows.map((t) => (
              <TaskRowView key={t.id} task={t} today={today} />
            ))}
          </ul>
        )}
      </SectionCard>

      <SectionCard
        title="Needs you"
        count={attn.isLoading ? "loading…" : `${attnItems.length} open`}
      >
        {attnItems.length === 0 ? (
          <p className="py-2 text-sm text-muted-foreground">Nothing is waiting on you.</p>
        ) : (
          <ul>
            {attnItems.slice(0, 8).map((i) => (
              <AttnRow key={i.id} item={i} />
            ))}
          </ul>
        )}
      </SectionCard>

      <SectionCard
        title="Issues · all repos"
        count={
          issues.isLoading
            ? "loading…"
            : `${issues.data?.count ?? 0} open${degraded > 0 ? ` · ${degraded} repo(s) degraded` : ""}`
        }
        action={
          <Button
            size="sm"
            variant="secondary"
            disabled={triage.isPending}
            onClick={() => triage.mutate()}
            title="rank everything and pick what's doable today"
          >
            <Play className="h-4 w-4" /> Triage for today
          </Button>
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
