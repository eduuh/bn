import { useMemo, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useNavigate } from "react-router-dom";
import {
  AlertTriangle,
  ArrowUpRight,
  Ban,
  CalendarClock,
  Check,
  Copy,
  CornerDownRight,
  ExternalLink,
  FileText,
  Play,
  Search,
  Sparkles,
} from "lucide-react";

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
  IssueRow,
  PipelineStageWork,
  RepoIssues,
  StageTask,
  TaskRow,
} from "../api/types";
import { attnAction, createTask, sendMessage, taskAction, taskAgent } from "../api/writes";
import { PipelineFlow } from "../components/PipelineFlow";
import { NewTaskDialog } from "../components/NewTaskDialog";
import { SteerButton } from "../components/SteerButton";
import { ToastStack } from "../components/ToastStack";
import { useContextMenu, type ContextMenuItem } from "../components/ContextMenu";
import { pushToast } from "../lib/toast";
import { Badge } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import { Card } from "../components/ui/card";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "../components/ui/tabs";
import { cn } from "../lib/utils";
import { Research } from "./Research";
import { TodayQueue } from "./TodayQueue";

/**
 * Has this task earned the right to be started?
 *
 * **Mirrors `actions::is_triaged` on the server, and is not the enforcement.** The endpoint answers
 * 409 `not_triaged` regardless of what this returns — the CLI and every agent POST to it too. This
 * exists so the operator is told *why* before clicking, not to decide whether it is allowed.
 *
 * Both halves are required, because two different agents contribute one each: `triage` picks what
 * is doable today and stamps `day:`, `enhance` (the improve agent) makes the task SMART and records
 * `smart:`. A future `day:` is deliberately not startable — it was triaged for another day.
 */
function isTriaged(task: TaskRow, today: string): boolean {
  return !!task.day && task.day <= today && task.smart === "ok";
}

/** Why a task cannot be started yet, phrased as the thing to do about it. */
function blockedReason(task: TaskRow, today: string): string {
  if (task.smart === "stuck") return "the enhance agent gave up — this one needs you";
  if (task.smart !== "ok") return "has not passed the SMART gate — run Enhance";
  if (!task.day) return "not scheduled — run Triage for today";
  if (task.day > today) return `triaged for ${task.day}, not today`;
  return "not triaged yet";
}

/** Copy to the clipboard when the environment has one (no-op under jsdom / older browsers). */
function copy(text: string) {
  if (typeof navigator !== "undefined" && navigator.clipboard) {
    void navigator.clipboard.writeText(text).catch(() => {});
  }
}

/** Tomorrow as `YYYY-MM-DD`, for a one-day snooze. */
function tomorrow(): string {
  return new Date(Date.now() + 86_400_000).toISOString().slice(0, 10);
}

/** The card the whole deck is built from: a bordered surface that lifts its border on hover, with a
 * main row and an optional provenance sub-row. */
const CARD =
  "group flex flex-col gap-1.5 rounded-lg border border-border bg-muted px-3 py-2.5 shadow-sm transition-colors hover:border-input hover:bg-accent";
const CARD_MAIN = "flex w-full flex-wrap items-center gap-x-2.5 gap-y-1";

/** The work-order token (#993) — A..Z, the order to work through; A is the one to do next. */
function OrderToken({ letter }: { letter?: string }) {
  if (!letter) return null;
  return (
    <span
      className={cn(
        "flex h-5 w-5 shrink-0 items-center justify-center rounded font-mono text-[11px] font-bold tabular-nums",
        letter === "A"
          ? "bg-primary text-primary-foreground"
          : "border border-border bg-background text-muted-foreground",
      )}
      title="work order — the order to work through; A is next"
    >
      {letter}
    </span>
  );
}

/** Where a task came from (`TaskRow.src`) — the provenance chip. */
function SourceChip({ src }: { src: string | null }) {
  if (!src) return null;
  return (
    <span
      className="inline-flex items-center gap-1 rounded-md border border-border bg-background px-1.5 py-0.5 font-mono text-[11px] text-muted-foreground"
      title={`source: ${src}`}
    >
      <CornerDownRight className="h-3 w-3 shrink-0 opacity-70" /> {src}
    </span>
  );
}

/** The task's document (`TaskRow.doc`) — clicks through to the detail page, which fetches and
 * renders the doc body (`## Done When`, the plan, …) as markdown. This is the "read task.md from the
 * browser" path. */
function DocChip({ doc, taskId }: { doc: string | null; taskId: string }) {
  if (!doc) return null;
  const name = doc.split("/").pop() || doc;
  return (
    <Link
      to={`/tasks/${taskId}`}
      className="inline-flex items-center gap-1 font-mono text-[11px] text-muted-foreground hover:text-primary"
      title={`open ${doc} — read it in the browser`}
    >
      <FileText className="h-3 w-3 shrink-0 opacity-70" /> {name}
    </Link>
  );
}

/** The provenance sub-row — only present when the task actually carries a source or a doc. */
function ProvSub({ task }: { task: TaskRow }) {
  if (!task.src && !task.doc) return null;
  return (
    <div className="flex w-full flex-wrap items-center gap-x-2 gap-y-1 pl-7">
      <SourceChip src={task.src} />
      <DocChip doc={task.doc} taskId={task.id} />
    </div>
  );
}

/** The shared leading cells of a task row: id, summary, repo, day. */
function RowHead({ task, today }: { task: TaskRow; today: string }) {
  const overdue = !!task.day && task.day < today;
  return (
    <>
      <span className="font-mono text-[11px] text-muted-foreground/80">{task.id}</span>
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

/** Prompt for a cancel reason, warning that a live agent is not stopped by cancelling the task. */
function cancelWithPrompt(id: string, agentTarget: string | undefined, run: (reason: string) => void) {
  const warn = agentTarget
    ? `\n\n${agentTarget} is still running on it and will NOT be stopped — stop it in the fleet.`
    : "";
  const reason = window.prompt(`Cancel ${id}?${warn}\n\nReason (optional):`);
  if (reason === null) return;
  run(reason);
}

/** The right-click actions common to every task, wired to real endpoints. `onStart` is only passed
 * for startable work; everything with no server endpoint yet is simply left out. */
function taskMenuItems(o: {
  id: string;
  navigate: (to: string) => void;
  onEnhance: () => void;
  onStart?: () => void;
  onCancel: () => void;
}): ContextMenuItem[] {
  const items: ContextMenuItem[] = [
    { label: "Open task", icon: ExternalLink, onSelect: () => o.navigate(`/tasks/${o.id}`) },
    { label: "Enhance", icon: Sparkles, onSelect: o.onEnhance },
  ];
  if (o.onStart) items.push({ label: "Start", icon: Play, onSelect: o.onStart });
  items.push({
    label: (
      <>
        Copy id <span className="font-mono text-muted-foreground">{o.id}</span>
      </>
    ),
    icon: Copy,
    onSelect: () => copy(o.id),
  });
  items.push({
    label: "Cancel task",
    icon: Ban,
    danger: true,
    separatorBefore: true,
    onSelect: o.onCancel,
  });
  return items;
}

/** A task that has been triaged: the only kind that offers Start. In the redesign these are the
 * *enhanced* tasks — their doc is past the SMART gate — so the badge reads `enhanced`. */
function ReadyRow({ task, today, letter }: { task: TaskRow; today: string; letter?: string }) {
  const qc = useQueryClient();
  const navigate = useNavigate();
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
  const enhance = useMutation({
    mutationFn: () => taskAgent(task.id, "improve"),
    onSuccess: () => pushToast(`Enhance agent started on ${task.id} — watch the fleet`, "success"),
    onError: (e) => pushToast(`Could not enhance: ${(e as Error).message}`, "error"),
  });
  const cancel = useMutation({
    mutationFn: (reason: string) => taskAction(task.id, "cancel", reason || undefined),
    onSuccess: () => {
      pushToast(`Cancelled ${task.id}`, "success");
      invalidate();
    },
    onError: (e) => pushToast(`Could not cancel: ${(e as Error).message}`, "error"),
  });
  const menu = useContextMenu(
    taskMenuItems({
      id: task.id,
      navigate,
      onEnhance: () => enhance.mutate(),
      onStart: () => start.mutate(),
      onCancel: () => cancelWithPrompt(task.id, undefined, (r) => cancel.mutate(r)),
    }),
    `actions for ${task.id}`,
  );

  return (
    <li className={CARD} onContextMenu={menu.onContextMenu}>
      <div className={CARD_MAIN}>
        <OrderToken letter={letter} />
        <RowHead task={task} today={today} />
        {task.est && (
          <span className="font-mono text-[11px] text-muted-foreground">{task.est}</span>
        )}
        <Badge variant="ok" title="enhanced — past the SMART gate and triaged for today">
          enhanced
        </Badge>
        <Button
          size="sm"
          disabled={start.isPending}
          onClick={() => start.mutate()}
          title="start this task — marks it wip"
        >
          <Play className="h-4 w-4" /> Start
        </Button>
      </div>
      <ProvSub task={task} />
      {menu.node}
    </li>
  );
}

/** A task in the **enhance queue** — not yet triaged. Start is present but disabled: an absent
 * button would read as "this task cannot be started ever", when the truth is "not yet, and here is
 * what to do". `Enhance` runs the improve agent that scopes it toward the SMART gate. */
function EnhanceQueueRow({ task, today, letter }: { task: TaskRow; today: string; letter?: string }) {
  const navigate = useNavigate();
  const qc = useQueryClient();
  const reason = blockedReason(task, today);
  const enhance = useMutation({
    mutationFn: (steer: string) => taskAgent(task.id, "improve", steer || undefined),
    onSuccess: () => pushToast(`Enhance agent started on ${task.id} — watch the fleet`, "success"),
    onError: (e) => pushToast(`Could not enhance: ${(e as Error).message}`, "error"),
  });
  const cancel = useMutation({
    mutationFn: (r: string) => taskAction(task.id, "cancel", r || undefined),
    onSuccess: () => {
      pushToast(`Cancelled ${task.id}`, "success");
      void qc.invalidateQueries({ queryKey: queryKeys.tasks("today") });
    },
    onError: (e) => pushToast(`Could not cancel: ${(e as Error).message}`, "error"),
  });
  const menu = useContextMenu(
    taskMenuItems({
      id: task.id,
      navigate,
      onEnhance: () => enhance.mutate(""),
      onCancel: () => cancelWithPrompt(task.id, undefined, (r) => cancel.mutate(r)),
    }),
    `actions for ${task.id}`,
  );

  return (
    <li className={CARD} onContextMenu={menu.onContextMenu}>
      <div className={CARD_MAIN}>
        <OrderToken letter={letter} />
        <RowHead task={task} today={today} />
        <Badge
          variant={task.smart === "stuck" ? "urgent" : task.smart === "fail" ? "warn" : "muted"}
          title={reason}
        >
          {task.smart === "stuck" ? "needs you" : task.smart === "fail" ? "not ready" : "unscoped"}
        </Badge>
        <SteerButton
          label="Enhance"
          title="run the enhance agent on this task"
          placeholder="things to look out for (optional)"
          busy={enhance.isPending}
          onSubmit={(steer) => enhance.mutate(steer)}
        />
        <Button size="sm" variant="secondary" disabled title={reason} className="opacity-50">
          <Play className="h-4 w-4" /> Start
        </Button>
      </div>
      <ProvSub task={task} />
      {menu.node}
    </li>
  );
}

/** Work in flight: its stage, whatever agent is on it, and the things you can do to it. */
function InFlightRow({ task, today, letter }: { task: StageTask; today: string; letter?: string }) {
  const qc = useQueryClient();
  const navigate = useNavigate();
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
  const enhance = useMutation({
    mutationFn: () => taskAgent(task.id, "improve"),
    onSuccess: () => pushToast(`Enhance agent started on ${task.id} — watch the fleet`, "success"),
    onError: (e) => pushToast(`Could not enhance: ${(e as Error).message}`, "error"),
  });
  const menu = useContextMenu(
    taskMenuItems({
      id: task.id,
      navigate,
      onEnhance: () => enhance.mutate(),
      onCancel: () => cancelWithPrompt(task.id, agent?.target, (r) => cancel.mutate(r)),
    }),
    `actions for ${task.id}`,
  );

  return (
    <li className={CARD} onContextMenu={menu.onContextMenu}>
      <div className={CARD_MAIN}>
        <OrderToken letter={letter} />
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
          <Badge variant="muted" className="font-mono" title={`running the \`${task.skill}\` workflow`}>
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
          onClick={() => cancelWithPrompt(task.id, agent?.target, (r) => cancel.mutate(r))}
        >
          <Ban className="h-4 w-4 text-urgent" />
        </Button>
      </div>
      <ProvSub task={task} />
      {menu.node}
    </li>
  );
}

/** One attention item — a finding that needs a human. Right-click to investigate, enqueue, snooze
 * or dismiss (all real `/api/attention/:id` verbs); the visible button keeps the common one to hand. */
function AttnRow({ item }: { item: AttnItem }) {
  const qc = useQueryClient();
  const invalidate = () => void qc.invalidateQueries({ queryKey: queryKeys.attn });
  const investigate = useMutation({
    // Investigation is a task agent too: it enhances the record before anything executes.
    mutationFn: (steer: string) => taskAgent(item.id, "improve", steer || undefined),
    onSuccess: () => pushToast(`Investigating ${item.id} — watch the fleet`, "success"),
    onError: (e) => pushToast(`Could not start: ${(e as Error).message}`, "error"),
  });
  const plan = useMutation({
    mutationFn: () => attnAction(item.id, "plan"),
    onSuccess: () => {
      pushToast(`Added ${item.id} to the enhance queue`, "success");
      invalidate();
    },
    onError: (e) => pushToast(`Could not enqueue: ${(e as Error).message}`, "error"),
  });
  const snooze = useMutation({
    mutationFn: () => attnAction(item.id, "snooze", { until: tomorrow() }),
    onSuccess: () => {
      pushToast(`Snoozed ${item.id} 1 day`, "success");
      invalidate();
    },
    onError: (e) => pushToast(`Could not snooze: ${(e as Error).message}`, "error"),
  });
  const dismiss = useMutation({
    mutationFn: () => attnAction(item.id, "remove"),
    onSuccess: () => {
      pushToast(`Dismissed ${item.id}`, "success");
      invalidate();
    },
    onError: (e) => pushToast(`Could not dismiss: ${(e as Error).message}`, "error"),
  });
  const tone = item.severity === "urgent" ? "urgent" : item.severity === "warn" ? "warn" : "info";
  const menu = useContextMenu(
    [
      { label: "Investigate", icon: Search, onSelect: () => investigate.mutate("") },
      { label: "Add to enhance queue", icon: Sparkles, onSelect: () => plan.mutate() },
      { label: "Snooze 1 day", icon: CalendarClock, onSelect: () => snooze.mutate() },
      { label: "Copy", icon: Copy, onSelect: () => copy(item.title) },
      { label: "Dismiss", icon: Ban, danger: true, separatorBefore: true, onSelect: () => dismiss.mutate() },
    ],
    `actions for ${item.id}`,
  );

  return (
    <li className={CARD} onContextMenu={menu.onContextMenu}>
      <div className={CARD_MAIN}>
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
      </div>
      {menu.node}
    </li>
  );
}

/** One finished/cancelled task — right-click to open its detail or copy its id. */
function DoneRow({ t }: { t: TaskRow & { reason?: string | null } }) {
  const navigate = useNavigate();
  const cancelled = t.status === "cancelled";
  const menu = useContextMenu(
    [
      { label: "Open task", icon: ExternalLink, onSelect: () => navigate(`/tasks/${t.id}`) },
      {
        label: (
          <>
            Copy id <span className="font-mono text-muted-foreground">{t.id}</span>
          </>
        ),
        icon: Copy,
        onSelect: () => copy(t.id),
      },
    ],
    `actions for ${t.id}`,
  );
  return (
    <li
      className="flex flex-wrap items-center gap-x-2.5 gap-y-1 rounded-md px-2 py-1.5 transition-colors hover:bg-accent"
      onContextMenu={menu.onContextMenu}
    >
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
      {t.repo && <span className="font-mono text-[11px] text-muted-foreground">[{t.repo}]</span>}
      {/* Cancelled work is shown but never counted as finished — the two are separate figures in the
          header for the same reason. */}
      {cancelled && t.reason && (
        <span className="font-mono text-[11px] text-muted-foreground/80">{t.reason}</span>
      )}
      {menu.node}
    </li>
  );
}

/** One issue — right-click to turn it into a task (into the enhance queue), open it, or copy its #. */
function IssueLine({ issue, repo }: { issue: IssueRow; repo: string }) {
  const qc = useQueryClient();
  const make = useMutation({
    mutationFn: () => createTask({ title: issue.title, repo }),
    onSuccess: (r) => {
      pushToast(`Created ${r.id} from #${issue.number} — the enhance agent will scope it`, "success");
      void qc.invalidateQueries({ queryKey: queryKeys.tasks("today") });
    },
    onError: (e) => pushToast(`Could not create: ${(e as Error).message}`, "error"),
  });
  const menu = useContextMenu(
    [
      { label: "Add to enhance queue", icon: Sparkles, onSelect: () => make.mutate() },
      { label: "Open issue", icon: ExternalLink, onSelect: () => window.open(issue.url, "_blank", "noreferrer") },
      {
        label: (
          <>
            Copy <span className="font-mono text-muted-foreground">#{issue.number}</span>
          </>
        ),
        icon: Copy,
        onSelect: () => copy(`#${issue.number}`),
      },
    ],
    `actions for issue #${issue.number}`,
  );
  return (
    <li
      className="flex items-center gap-2 rounded-md px-1 py-0.5 text-[12px] transition-colors hover:bg-accent"
      onContextMenu={menu.onContextMenu}
    >
      <span className="font-mono text-muted-foreground/80">#{issue.number}</span>
      <a
        href={issue.url}
        target="_blank"
        rel="noreferrer"
        className="min-w-0 flex-1 truncate text-foreground hover:text-primary"
      >
        {issue.title}
      </a>
      {issue.is_assigned && <span className="text-[10.5px] text-muted-foreground">assigned</span>}
      {menu.node}
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
          <span className="inline-flex items-center gap-1 text-[11px] text-warn" title={repo.error}>
            <AlertTriangle className="h-3 w-3" />
            {repo.stale ? `stale${age !== null ? `, fetched ${age}m ago` : ""}` : "could not fetch"}
          </span>
        )}
      </div>
      {repo.issues.length === 0 && !repo.error && (
        <p className="mt-0.5 text-[11px] text-muted-foreground">No open issues.</p>
      )}
      <ul className="mt-0.5">
        {repo.issues.slice(0, 8).map((issue) => (
          <IssueLine key={issue.url} issue={issue} repo={repo.repo} />
        ))}
      </ul>
    </div>
  );
}

/** The underline tab-trigger style (matches the redesign prototype) — a bottom-border indicator in
 * clay instead of the default pill. */
const TAB_TRIGGER =
  "rounded-none border-b-2 border-transparent bg-transparent px-3 py-2 text-muted-foreground shadow-none data-[state=active]:border-primary data-[state=active]:bg-transparent data-[state=active]:text-foreground data-[state=active]:shadow-none";

function SectionCard({
  eyebrow,
  title,
  count,
  action,
  tone,
  children,
}: {
  eyebrow?: string;
  title: string;
  count?: string;
  action?: React.ReactNode;
  tone?: "ready";
  children: React.ReactNode;
}) {
  return (
    <Card className={cn("p-3.5", tone === "ready" && "border-ok/40 bg-ok/[0.05]")}>
      <div className="mb-2.5 flex flex-wrap items-baseline gap-x-2.5 gap-y-1">
        {eyebrow && (
          <span className="font-mono text-[10px] font-medium uppercase tracking-[0.14em] text-muted-foreground/70">
            {eyebrow}
          </span>
        )}
        <h2 className="text-sm font-semibold tracking-tight text-foreground">{title}</h2>
        {count && (
          <span className="font-mono text-[11px] tabular-nums text-muted-foreground/70">{count}</span>
        )}
        {action && <span className="ml-auto">{action}</span>}
      </div>
      {children}
    </Card>
  );
}

/**
 * `/` — the task-first home (#1006, EPIC #993), reorganised as **the task lifecycle** (#1033) and
 * given the deck redesign: each task is a card carrying its work-order letter, its provenance
 * (`src`) and doc, and a right-click menu of the real transitions out of its state. The untriaged
 * pile is the **enhance queue** — where the improve agent scopes work toward the SMART gate — so
 * everything in *Ready to start* is already enhanced.
 *
 * Start appears only on triaged work, which the server enforces independently (409 `not_triaged`),
 * because the CLI and every agent hit the same endpoint and a gate only the dashboard honours is a
 * suggestion. The in-flight rows and pipeline counts come from a single projection joined on
 * `(repo, branch)`, so a node's number and the rows beneath it cannot disagree.
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
    : [...stageList.flatMap((s) => s.tasks), ...wip.filter((t) => !stagedIds.has(t.id))];

  // The work-order letters (#993): a single A..Z run down the actionable deck in reading order —
  // Ready first (A is next), then in flight, then the enhance queue.
  const orderOf = useMemo(() => {
    const m = new Map<string, string>();
    let i = 0;
    for (const t of [...ready, ...inFlightRows, ...untriaged]) {
      if (i < 26 && !m.has(t.id)) m.set(t.id, String.fromCharCode(65 + i++));
    }
    return m;
  }, [ready, inFlightRows, untriaged]);

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
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div className="space-y-1">
          <div className="flex items-baseline gap-3">
            <h1 className="text-xl font-semibold tracking-tight text-foreground">Today</h1>
            <span className="font-mono text-[11px] tabular-nums text-muted-foreground/70">
              {today}
            </span>
          </div>
          <p className="text-sm text-muted-foreground">
            What can I start, what's already moving, and where it came from.
          </p>
        </div>
        <span className="flex items-center gap-2">
          {triageButton}
          <Button size="sm" onClick={() => setCreating(true)}>
            <Sparkles className="h-4 w-4" /> New task
          </Button>
        </span>
      </div>

      <Tabs defaultValue="deck" className="w-full">
        <TabsList className="h-auto w-full justify-start gap-1 rounded-none border-b border-border bg-transparent p-0">
          <TabsTrigger value="deck" className={TAB_TRIGGER}>
            Task deck
          </TabsTrigger>
          <TabsTrigger value="queue" className={TAB_TRIGGER}>
            Today's queue
          </TabsTrigger>
          <TabsTrigger value="research" className={TAB_TRIGGER}>
            Research
          </TabsTrigger>
        </TabsList>

        <TabsContent value="deck" className="space-y-4">
          <SectionCard
            eyebrow="Start now"
            title="Ready to start"
            tone="ready"
            count={
              tasks.isLoading
                ? "loading…"
                : `${ready.length} triaged · enhanced · the only tasks that can be started`
            }
          >
            {ready.length === 0 ? (
              <p className="py-2 text-sm text-muted-foreground">
                Nothing is ready — run <span className="font-medium">Triage for today</span> to pick
                what's doable, or <span className="font-medium">Enhance</span> a task past the SMART
                gate.
              </p>
            ) : (
              <ul className="space-y-2">
                {ready.map((t) => (
                  <ReadyRow key={t.id} task={t} today={today} letter={orderOf.get(t.id)} />
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
              <ul className="space-y-2">
                {inFlightRows.map((t) => (
                  <InFlightRow key={t.id} task={t} today={today} letter={orderOf.get(t.id)} />
                ))}
              </ul>
            )}

            {/* A worktree at a stage with no task behind it. Named rather than dropped: it is exactly
                the gap #993 closes, and hiding it would make the count read as wrong instead of as a
                fact about the pipeline. */}
            {selected?.orphans.map((o) => (
              <p key={`${o.repo}/${o.branch}`} className="mt-1.5 text-[11px] text-muted-foreground">
                ⌁{" "}
                <span className="font-mono text-foreground">
                  {o.repo}/{o.branch}
                </span>{" "}
                is in {selected.id} with no task attached.
              </p>
            ))}
          </SectionCard>

          <SectionCard
            eyebrow="Autonomous"
            title="Enhance queue"
            count={tasks.isLoading ? "loading…" : `${untriaged.length} waiting · cannot be started`}
            action={triageButton}
          >
            {untriaged.length === 0 ? (
              <p className="py-2 text-sm text-muted-foreground">
                Everything open has been enhanced and triaged.
              </p>
            ) : (
              <ul className="space-y-2">
                {untriaged.map((t) => (
                  <EnhanceQueueRow key={t.id} task={t} today={today} letter={orderOf.get(t.id)} />
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
              <ul className="space-y-0.5">
                {doneRows.map((t) => (
                  <DoneRow key={t.id} t={t} />
                ))}
              </ul>
            )}
          </SectionCard>

          <SectionCard
            title="Needs you"
            count={attn.isLoading ? "loading…" : `${attnItems.length} open · most severe first`}
            action={
              <a href="/attention" className="inline-flex items-center gap-0.5 text-[11px] text-primary">
                all {attnItems.length} <ArrowUpRight className="h-3 w-3" />
              </a>
            }
          >
            {attnItems.length === 0 ? (
              <p className="py-2 text-sm text-muted-foreground">Nothing is waiting on you.</p>
            ) : (
              <>
                <ul className="space-y-2">
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
                : `${issues.data?.count ?? 0} open${
                    degraded > 0 ? ` · ${degraded} repo(s) degraded` : ""
                  } · right-click to enqueue`
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
            <a href="/repos" className="mt-2 inline-flex items-center gap-0.5 text-[11px] text-primary">
              all repos <ArrowUpRight className="h-3 w-3" />
            </a>
          </SectionCard>
        </TabsContent>

        <TabsContent value="queue">
          <TodayQueue />
        </TabsContent>
        <TabsContent value="research">
          <Research />
        </TabsContent>
      </Tabs>

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
