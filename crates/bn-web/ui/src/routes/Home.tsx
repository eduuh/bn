import { useMemo, useState, type ReactNode } from "react";
import { useQuery } from "@tanstack/react-query";
import { Link, useNavigate } from "react-router-dom";
import {
  AlertTriangle,
  ArrowUpRight,
  Ban,
  CalendarClock,
  Copy,
  CornerDownRight,
  ExternalLink,
  FileText,
  Play,
  Search,
  Sparkles,
} from "lucide-react";

import { attnQuery, doneTasksQuery, issuesQuery, tasksQuery } from "../api/client";
import type { AttnItem, IssueRow, RepoIssues, TaskRow } from "../api/types";
import { attnAction, createTask, taskAction, taskAgent } from "../api/writes";
import { useAppMutation } from "../hooks/useAppMutation";
import { affects } from "../lib/invalidation";
import { dropAttn, dropActionForAttn, dropTask, patchTask } from "../lib/optimistic";
import { NewTaskDialog } from "../components/NewTaskDialog";
import { Empty, Hint, RowsSkeleton, SectionCard } from "../components/SectionCard";
import { SteerButton } from "../components/SteerButton";
import { ToastStack } from "../components/ToastStack";
import {
  AlertDialog,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "../components/ui/alert-dialog";
import { Badge } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import { buttonVariants } from "../components/ui/button-variants";
import { Card } from "../components/ui/card";
import {
  ContextMenu,
  ContextMenuContent,
  ContextMenuItem,
  ContextMenuLabel,
  ContextMenuSeparator,
  ContextMenuShortcut,
  ContextMenuTrigger,
} from "../components/ui/context-menu";
import { Input } from "../components/ui/input";
import { Label } from "../components/ui/label";
import { Separator } from "../components/ui/separator";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "../components/ui/tabs";
import { TooltipProvider } from "../components/ui/tooltip";
import { cn } from "../lib/utils";
import { Research } from "./Research";

/**
 * Has this task earned the right to be started?
 *
 * **Mirrors `actions::is_triaged` on the server, and is not the enforcement.** The endpoint answers
 * 409 `not_triaged` regardless of what this returns — the CLI and every agent POST to it too. This
 * exists so the operator is told *why* before clicking, not to decide whether it is allowed.
 *
 * Both halves are required, because two different agents contribute one each: `triage` picks what
 * is doable today and stamps `day:`, `enhance` makes the task SMART and records
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

/**
 * Open a dialog from a menu item.
 *
 * Radix closes the menu on `onSelect` and restores focus to the trigger in the same tick. Flipping
 * a dialog open inside that tick races the restore — the dialog mounts, the menu's focus return
 * steals it back, and the reason field ends up unfocused (occasionally with `pointer-events: none`
 * still on `<body>`). Deferring by one turn of the event loop lets the menu finish closing first.
 */
function openAfterMenu(open: () => void) {
  window.setTimeout(open, 0);
}

/** The work-order token (#993) — A..Z, the order to work through; A is the one to do next. */
function OrderToken({ letter }: { letter?: string }) {
  if (!letter) return null;
  return (
    <Hint label="Work order — the order to work through. A is next.">
      <span
        className={cn(
          "flex h-5 w-5 shrink-0 items-center justify-center rounded font-mono text-[11px] font-bold tabular-nums",
          letter === "A"
            ? "bg-primary text-primary-foreground"
            : "border border-border bg-background text-muted-foreground",
        )}
      >
        {letter}
      </span>
    </Hint>
  );
}

/** The provenance sub-row — where a task came from (`src`) and its doc. Only rendered when the task
 * actually carries one, so an empty row never reserves space. */
function Provenance({ task }: { task: TaskRow }) {
  if (!task.src && !task.doc) return null;
  const docName = task.doc ? task.doc.split("/").pop() || task.doc : null;
  return (
    <div className="flex w-full flex-wrap items-center gap-x-2 gap-y-1 pl-7">
      {task.src && (
        <Hint label={`Source: ${task.src}`}>
          <span className="inline-flex items-center gap-1 rounded-md border border-border bg-background px-1.5 py-0.5 font-mono text-[11px] text-muted-foreground">
            <CornerDownRight className="h-3 w-3 shrink-0 opacity-70" /> {task.src}
          </span>
        </Hint>
      )}
      {task.src && task.doc && <Separator orientation="vertical" className="h-3" />}
      {docName && (
        // Clicks through to the detail page, which fetches and renders the doc body (`## Done When`,
        // the plan, …) as markdown. This is the "read task.md from the browser" path.
        <Hint label={`Open ${task.doc} — read it in the browser`}>
          <Link
            to={`/tasks/${task.id}`}
            className="inline-flex items-center gap-1 font-mono text-[11px] text-muted-foreground hover:text-primary"
          >
            <FileText className="h-3 w-3 shrink-0 opacity-70" /> {docName}
          </Link>
        </Hint>
      )}
    </div>
  );
}

/** The shared leading cells of a task row: id, summary, repo, day. */
function TaskMeta({ task, today }: { task: TaskRow; today: string }) {
  const overdue = !!task.day && task.day < today;
  return (
    <>
      <span className="font-mono text-[11px] text-muted-foreground/80">{task.id}</span>
      <span className="min-w-0 flex-1 text-sm leading-snug text-foreground">{task.summary}</span>
      {task.repo && (
        <span className="font-mono text-[11px] text-muted-foreground">[{task.repo}]</span>
      )}
      {task.day &&
        (overdue ? (
          <Hint label="Carried over from an earlier day.">
            <span className="font-mono text-[11px] font-semibold text-warn">{task.day}!</span>
          </Hint>
        ) : (
          <span className="font-mono text-[11px] text-muted-foreground">{task.day}</span>
        ))}
    </>
  );
}

/**
 * Confirm a cancel and collect the reason.
 *
 * This replaces a `window.prompt`, which was wrong in two ways beyond looking out of place: the
 * native prompt cannot render the warning below as anything but more prompt text, and it blocks the
 * whole tab while the dashboard's SSE stream keeps pushing.
 *
 * The warning is the important part. Cancelling the *task* does not stop an agent working on it —
 * those are two different lifetimes — so an operator who cancels and walks away would leave a
 * process running against a task nobody is tracking.
 */
function CancelTaskDialog({
  id,
  agentTarget,
  open,
  onOpenChange,
  onConfirm,
}: {
  id: string;
  agentTarget?: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onConfirm: (reason: string) => void;
}) {
  const [reason, setReason] = useState("");
  return (
    <AlertDialog
      open={open}
      onOpenChange={(next) => {
        if (!next) setReason("");
        onOpenChange(next);
      }}
    >
      <AlertDialogContent className="max-w-sm">
        <AlertDialogHeader>
          <AlertDialogTitle>
            Cancel <span className="font-mono text-[15px]">{id}</span>?
          </AlertDialogTitle>
          <AlertDialogDescription>
            {agentTarget ? (
              <>
                <span className="font-mono text-warn">{agentTarget}</span> is still running on it
                and will <strong className="font-semibold text-warn">not</strong> be stopped — stop
                it in the fleet.
              </>
            ) : (
              "The task is closed as cancelled. It stays in the store and in Done today."
            )}
          </AlertDialogDescription>
        </AlertDialogHeader>

        <div className="space-y-1.5">
          <Label htmlFor={`cancel-reason-${id}`} className="text-[11px]">
            Reason (optional)
          </Label>
          <Input
            id={`cancel-reason-${id}`}
            autoFocus
            value={reason}
            placeholder="superseded by #1044"
            onChange={(e) => setReason(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                onConfirm(reason.trim());
                onOpenChange(false);
              }
            }}
          />
        </div>

        <AlertDialogFooter>
          <AlertDialogCancel>Keep it</AlertDialogCancel>
          {/* Not `AlertDialogAction`: that renders the primary button, and cancelling is the
              destructive branch — it should not look like the safe default. */}
          <AlertDialogCancel
            className={buttonVariants({ variant: "destructive", size: "sm" })}
            onClick={() => onConfirm(reason.trim())}
          >
            Cancel task
          </AlertDialogCancel>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}

/** The right-click actions common to every task, wired to real endpoints. `onStart` is only passed
 * for startable work; everything with no server endpoint yet is simply left out, so the menu only
 * ever offers things that actually work. */
function TaskMenu({
  id,
  onEnhance,
  onStart,
  onCancel,
}: {
  id: string;
  onEnhance: () => void;
  onStart?: () => void;
  onCancel: () => void;
}) {
  const navigate = useNavigate();
  return (
    <ContextMenuContent>
      <ContextMenuLabel>{id}</ContextMenuLabel>
      <ContextMenuItem onSelect={() => navigate(`/tasks/${id}`)}>
        <ExternalLink /> Open task
      </ContextMenuItem>
      <ContextMenuItem onSelect={onEnhance}>
        <Sparkles /> Enhance
      </ContextMenuItem>
      {onStart && (
        <ContextMenuItem onSelect={onStart}>
          <Play /> Start
        </ContextMenuItem>
      )}
      <ContextMenuItem onSelect={() => copy(id)}>
        <Copy /> Copy id
        <ContextMenuShortcut>{id}</ContextMenuShortcut>
      </ContextMenuItem>
      <ContextMenuSeparator />
      <ContextMenuItem danger onSelect={() => openAfterMenu(onCancel)}>
        <Ban /> Cancel task
      </ContextMenuItem>
    </ContextMenuContent>
  );
}

/**
 * The card the whole deck is built from.
 *
 * A real `Card` rather than a class-string constant, so a row inherits the same surface, radius and
 * shadow tokens as every other card on the dashboard and only overrides what makes it a *row*: a
 * flatter `bg-muted` fill (it sits inside a section card) and a border that lifts on hover.
 */
function TaskCard({
  task,
  today,
  letter,
  menu,
  children,
}: {
  task: TaskRow;
  today: string;
  letter?: string;
  /** The `<ContextMenuContent>` for this row. */
  menu: ReactNode;
  /** State-specific chips and actions, appended to the main row. */
  children: ReactNode;
}) {
  return (
    <li>
      <ContextMenu>
        <ContextMenuTrigger asChild>
          <Card className="flex flex-col gap-1.5 border-border bg-muted px-3 py-2.5 shadow-sm transition-colors hover:border-input hover:bg-accent">
            <div className="flex w-full flex-wrap items-center gap-x-2.5 gap-y-1">
              <OrderToken letter={letter} />
              <TaskMeta task={task} today={today} />
              {children}
            </div>
            <Provenance task={task} />
          </Card>
        </ContextMenuTrigger>
        {menu}
      </ContextMenu>
    </li>
  );
}

/** A task that has been triaged: the only kind that offers Start. In the redesign these are the
 * *enhanced* tasks — their doc is past the SMART gate — so the badge reads `enhanced`. */
function ReadyRow({ task, today, letter }: { task: TaskRow; today: string; letter?: string }) {
  const [cancelling, setCancelling] = useState(false);
  const start = useAppMutation({
    mutationFn: () => taskAction(task.id, "wip"),
    optimistic: patchTask("today", task.id, { status: "wip" }),
    invalidates: affects.task,
    // The gate lives on the server, so a refusal can still arrive here — say so plainly rather
    // than letting the row look broken. The rollback puts the row back as it was.
    toast: { success: `Started ${task.id}`, error: `Could not start ${task.id}` },
  });
  const enhance = useAppMutation({
    mutationFn: () => taskAgent(task.id, "enhance"),
    invalidates: affects.agentLaunch,
    toast: {
      success: `Enhance agent started on ${task.id} — watch the fleet`,
      error: "Could not enhance",
    },
  });
  const cancel = useAppMutation<unknown, string>({
    mutationFn: (reason) => taskAction(task.id, "cancel", reason || undefined),
    optimistic: dropTask("today", task.id),
    invalidates: affects.task,
    toast: { success: `Cancelled ${task.id}`, error: "Could not cancel" },
  });

  return (
    <>
      <TaskCard
        task={task}
        today={today}
        letter={letter}
        menu={
          <TaskMenu
            id={task.id}
            onEnhance={() => enhance.mutate()}
            onStart={() => start.mutate()}
            onCancel={() => setCancelling(true)}
          />
        }
      >
        {task.est && (
          <span className="font-mono text-[11px] text-muted-foreground">{task.est}</span>
        )}
        <Hint label="Enhanced — past the SMART gate and triaged for today.">
          <Badge variant="ok">enhanced</Badge>
        </Hint>
        <Button size="sm" disabled={start.isPending} onClick={() => start.mutate()}>
          <Play /> Start
        </Button>
      </TaskCard>
      <CancelTaskDialog
        id={task.id}
        open={cancelling}
        onOpenChange={setCancelling}
        onConfirm={(r) => cancel.mutate(r)}
      />
    </>
  );
}

/** A task in the **enhance queue** — not yet triaged. Start is present but disabled: an absent
 * button would read as "this task cannot be started ever", when the truth is "not yet, and here is
 * what to do". `Enhance` scopes it toward the SMART gate. */
function EnhanceQueueRow({
  task,
  today,
  letter,
}: {
  task: TaskRow;
  today: string;
  letter?: string;
}) {
  const [cancelling, setCancelling] = useState(false);
  const reason = blockedReason(task, today);
  const enhance = useAppMutation<unknown, string>({
    mutationFn: (steer) => taskAgent(task.id, "enhance", steer || undefined),
    invalidates: affects.agentLaunch,
    toast: {
      success: `Enhance agent started on ${task.id} — watch the fleet`,
      error: "Could not enhance",
    },
  });
  const cancel = useAppMutation<unknown, string>({
    mutationFn: (r) => taskAction(task.id, "cancel", r || undefined),
    optimistic: dropTask("today", task.id),
    invalidates: affects.task,
    toast: { success: `Cancelled ${task.id}`, error: "Could not cancel" },
  });

  return (
    <>
      <TaskCard
        task={task}
        today={today}
        letter={letter}
        menu={
          <TaskMenu
            id={task.id}
            onEnhance={() => enhance.mutate("")}
            onCancel={() => setCancelling(true)}
          />
        }
      >
        <Hint label={reason}>
          <Badge
            variant={task.smart === "stuck" ? "urgent" : task.smart === "fail" ? "warn" : "muted"}
          >
            {task.smart === "stuck"
              ? "needs you"
              : task.smart === "fail"
                ? "not ready"
                : "unscoped"}
          </Badge>
        </Hint>
        <SteerButton
          label="Enhance"
          icon={Sparkles}
          title="run the enhance agent on this task"
          placeholder="things to look out for (optional)"
          busy={enhance.isPending}
          onSubmit={(steer) => enhance.mutate(steer)}
        />
        {/* A disabled button fires no pointer events, so the tooltip needs a live wrapper — and the
            wrapper is focusable so the reason is reachable from the keyboard too. */}
        <Hint label={reason}>
          <span tabIndex={0} className="inline-flex rounded-md">
            <Button size="sm" variant="secondary" disabled className="opacity-50">
              <Play /> Start
            </Button>
          </span>
        </Hint>
      </TaskCard>
      <CancelTaskDialog
        id={task.id}
        open={cancelling}
        onOpenChange={setCancelling}
        onConfirm={(r) => cancel.mutate(r)}
      />
    </>
  );
}

/** One attention item — a finding that needs a human. Right-click to investigate, enqueue, snooze
 * or dismiss (all real `/api/attention/:id` verbs); the visible button keeps the common one to hand. */
function AttnRow({ item }: { item: AttnItem }) {
  const investigate = useAppMutation<unknown, string>({
    // Investigation is a task agent too: it enhances the record before anything executes.
    mutationFn: (steer) => taskAgent(item.id, "enhance", steer || undefined),
    invalidates: affects.agentLaunch,
    toast: { success: `Investigating ${item.id} — watch the fleet`, error: "Could not start" },
  });
  // Enqueue, snooze and dismiss all take the row off today's list, so it goes on the click.
  const resolved = [dropAttn(item.id), dropActionForAttn(item.id)];
  const plan = useAppMutation({
    mutationFn: () => attnAction(item.id, "plan"),
    optimistic: resolved,
    invalidates: affects.attention,
    toast: { success: `Added ${item.id} to the enhance queue`, error: "Could not enqueue" },
  });
  const snooze = useAppMutation({
    mutationFn: () => attnAction(item.id, "snooze", { until: tomorrow() }),
    optimistic: resolved,
    invalidates: affects.attention,
    toast: { success: `Snoozed ${item.id} 1 day`, error: "Could not snooze" },
  });
  const dismiss = useAppMutation({
    mutationFn: () => attnAction(item.id, "remove"),
    optimistic: resolved,
    invalidates: affects.attention,
    toast: { success: `Dismissed ${item.id}`, error: "Could not dismiss" },
  });
  const tone = item.severity === "urgent" ? "urgent" : item.severity === "warn" ? "warn" : "info";

  return (
    <li>
      <ContextMenu>
        <ContextMenuTrigger asChild>
          <Card className="flex w-full flex-wrap items-center gap-x-2.5 gap-y-1 border-border bg-muted px-3 py-2.5 shadow-sm transition-colors hover:border-input hover:bg-accent">
            <Badge variant={tone} className="uppercase tracking-wide">
              {item.severity}
            </Badge>
            <span className="min-w-0 flex-1 text-sm leading-snug text-foreground">
              {item.title}
            </span>
            {item.repo && (
              <span className="font-mono text-[11px] text-muted-foreground">
                {item.repo}
                {item.branch ? `/${item.branch}` : ""}
              </span>
            )}
            <SteerButton
              label="Investigate"
              icon={Search}
              title="run an investigation agent, steered"
              placeholder="things to look out for (optional)"
              busy={investigate.isPending}
              onSubmit={(steer) => investigate.mutate(steer)}
            />
          </Card>
        </ContextMenuTrigger>
        <ContextMenuContent>
          <ContextMenuLabel>{item.id}</ContextMenuLabel>
          <ContextMenuItem onSelect={() => investigate.mutate("")}>
            <Search /> Investigate
          </ContextMenuItem>
          <ContextMenuItem onSelect={() => plan.mutate()}>
            <Sparkles /> Add to enhance queue
          </ContextMenuItem>
          <ContextMenuItem onSelect={() => snooze.mutate()}>
            <CalendarClock /> Snooze 1 day
          </ContextMenuItem>
          <ContextMenuItem onSelect={() => copy(item.title)}>
            <Copy /> Copy
          </ContextMenuItem>
          <ContextMenuSeparator />
          <ContextMenuItem danger onSelect={() => dismiss.mutate()}>
            <Ban /> Dismiss
          </ContextMenuItem>
        </ContextMenuContent>
      </ContextMenu>
    </li>
  );
}

/** One finished/cancelled task — right-click to open its detail or copy its id. */
function DoneRow({ t }: { t: TaskRow & { reason?: string | null } }) {
  const navigate = useNavigate();
  const cancelled = t.status === "cancelled";
  return (
    <li>
      <ContextMenu>
        <ContextMenuTrigger asChild>
          <div className="flex flex-wrap items-center gap-x-2.5 gap-y-1 rounded-md px-2 py-1.5 transition-colors hover:bg-accent">
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
            {/* Cancelled work is shown but never counted as finished — the two are separate figures
                in the header for the same reason. */}
            {cancelled && t.reason && (
              <span className="font-mono text-[11px] text-muted-foreground/80">{t.reason}</span>
            )}
          </div>
        </ContextMenuTrigger>
        <ContextMenuContent>
          <ContextMenuItem onSelect={() => navigate(`/tasks/${t.id}`)}>
            <ExternalLink /> Open task
          </ContextMenuItem>
          <ContextMenuItem onSelect={() => copy(t.id)}>
            <Copy /> Copy id
            <ContextMenuShortcut>{t.id}</ContextMenuShortcut>
          </ContextMenuItem>
        </ContextMenuContent>
      </ContextMenu>
    </li>
  );
}

/** One issue — right-click to turn it into a task (into the enhance queue), open it, or copy its #. */
function IssueLine({ issue, repo }: { issue: IssueRow; repo: string }) {
  const make = useAppMutation({
    mutationFn: () => createTask({ title: issue.title, repo }),
    invalidates: affects.task,
    toast: {
      success: (r) => `Created ${r.id} from #${issue.number} — the enhance agent will scope it`,
      error: "Could not create",
    },
  });

  return (
    <li>
      <ContextMenu>
        <ContextMenuTrigger asChild>
          <div className="flex items-center gap-2 rounded-md px-1 py-0.5 text-[12px] transition-colors hover:bg-accent">
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
              <Badge variant="muted" className="px-1.5 text-[10px]">
                assigned
              </Badge>
            )}
          </div>
        </ContextMenuTrigger>
        <ContextMenuContent>
          <ContextMenuItem onSelect={() => make.mutate()}>
            <Sparkles /> Add to enhance queue
          </ContextMenuItem>
          <ContextMenuItem onSelect={() => window.open(issue.url, "_blank", "noreferrer")}>
            <ExternalLink /> Open issue
          </ContextMenuItem>
          <ContextMenuItem onSelect={() => copy(`#${issue.number}`)}>
            <Copy /> Copy
            <ContextMenuShortcut>#{issue.number}</ContextMenuShortcut>
          </ContextMenuItem>
        </ContextMenuContent>
      </ContextMenu>
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
      <div className="flex flex-wrap items-center gap-2">
        <span className="font-mono text-[12px] font-semibold text-foreground">{repo.repo}</span>
        <span className="text-[11px] text-muted-foreground">{repo.issues.length} open</span>
        {/* The honesty contract from #999, surfaced: an empty list with an error is NOT the same
            fact as an empty backlog, so never let them render alike. */}
        {repo.error && (
          <Hint label={repo.error}>
            <Badge variant="warn">
              <AlertTriangle className="h-3 w-3" />
              {repo.stale
                ? `stale${age !== null ? `, fetched ${age}m ago` : ""}`
                : "could not fetch"}
            </Badge>
          </Hint>
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

/**
 * `/` — the task-first home (#1006, EPIC #993), reorganised as **the task lifecycle** (#1033) and
 * given the deck redesign: each task is a card carrying its work-order letter, its provenance
 * (`src`) and doc, and a right-click menu of the real transitions out of its state. The untriaged
 * pile is the **enhance queue** — where the enhance agent scopes work toward the SMART gate — so
 * everything in *Ready to start* is already enhanced.
 *
 * Start appears only on triaged work, which the server enforces independently (409 `not_triaged`),
 * because the CLI and every agent hit the same endpoint and a gate only the dashboard honours is a
 * suggestion.
 *
 * The screen is built entirely from the shadcn kit in `components/ui`: sections and rows are
 * `Card`s, the right-click menus are Radix `ContextMenu`s, the cancel confirmation is an
 * `AlertDialog` (it used to be a `window.prompt`), steering opens a `Popover`, and every "why is
 * this like that" caption is a `Tooltip` instead of a native `title` the phone can never show.
 */
export function Home() {
  const tasks = useQuery(tasksQuery("today"));
  const done = useQuery(doneTasksQuery());
  const attn = useQuery(attnQuery());
  const issues = useQuery(issuesQuery());

  const [creating, setCreating] = useState(false);

  const today = tasks.data?.today ?? "";
  const openTasks = useMemo(() => tasks.data?.tasks ?? [], [tasks.data]);

  // Partition the open store by lifecycle state. Work already in flight (`wip`) has its own screens
  // — it is not part of the deck, which is about what to pick up next.
  const { ready, untriaged } = useMemo(() => {
    const ready: TaskRow[] = [];
    const untriaged: TaskRow[] = [];
    for (const t of openTasks) {
      if (t.status === "wip") continue;
      if (isTriaged(t, today)) ready.push(t);
      else untriaged.push(t);
    }
    return { ready, untriaged };
  }, [openTasks, today]);

  // The work-order letters (#993): a single A..Z run down the actionable deck in reading order —
  // Ready first (A is next), then the enhance queue.
  const orderOf = useMemo(() => {
    const m = new Map<string, string>();
    let i = 0;
    for (const t of [...ready, ...untriaged]) {
      if (i < 26 && !m.has(t.id)) m.set(t.id, String.fromCharCode(65 + i++));
    }
    return m;
  }, [ready, untriaged]);

  const create = useAppMutation({
    mutationFn: createTask,
    invalidates: affects.task,
    toast: { success: (r) => `Created ${r.id}`, error: "Could not create" },
    onSuccess: () => setCreating(false),
  });
  const triage = useAppMutation({
    mutationFn: () => taskAgent("t0", "triage"),
    invalidates: affects.agentLaunch,
    toast: { success: "Triage started — watch the fleet", error: "Could not start triage" },
  });

  const triageButton = (
    <Button
      size="sm"
      variant="secondary"
      disabled={triage.isPending}
      onClick={() => triage.mutate()}
      title="rank everything and pick what's doable today"
    >
      <Play /> Triage for today
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

  const tabTrigger =
    "rounded-none border-b-2 border-transparent bg-transparent px-3 py-2 text-muted-foreground shadow-none data-[state=active]:border-primary data-[state=active]:bg-transparent data-[state=active]:text-foreground data-[state=active]:shadow-none";

  return (
    <TooltipProvider delayDuration={250}>
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
          <div className="flex items-center gap-2">
            {triageButton}
            <Button size="sm" onClick={() => setCreating(true)}>
              <Sparkles /> New task
            </Button>
          </div>
        </div>

        <Tabs defaultValue="deck" className="w-full">
          <TabsList className="h-auto w-full justify-start gap-1 rounded-none border-b border-border bg-transparent p-0">
            <TabsTrigger value="deck" className={tabTrigger}>
              Task deck
            </TabsTrigger>
            <TabsTrigger value="research" className={tabTrigger}>
              Research
            </TabsTrigger>
          </TabsList>

          <TabsContent value="deck" className="space-y-4">
            <SectionCard
              eyebrow="Start now"
              title="Ready to start"
              tone="ready"
              count={`${ready.length} triaged · enhanced · the only tasks that can be started`}
            >
              {tasks.isLoading ? (
                <RowsSkeleton />
              ) : ready.length === 0 ? (
                <Empty>
                  Nothing is ready — run <span className="font-medium">Triage for today</span> to
                  pick what's doable, or <span className="font-medium">Enhance</span> a task past
                  the SMART gate.
                </Empty>
              ) : (
                <ul className="space-y-2">
                  {ready.map((t) => (
                    <ReadyRow key={t.id} task={t} today={today} letter={orderOf.get(t.id)} />
                  ))}
                </ul>
              )}
            </SectionCard>

            <SectionCard
              eyebrow="Autonomous"
              title="Enhance queue"
              count={`${untriaged.length} waiting · cannot be started`}
              action={triageButton}
            >
              {tasks.isLoading ? (
                <RowsSkeleton rows={2} />
              ) : untriaged.length === 0 ? (
                <Empty>Everything open has been enhanced and triaged.</Empty>
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
              count={`${done.data?.done ?? 0} finished${
                (done.data?.cancelled ?? 0) > 0 ? ` · ${done.data?.cancelled} cancelled` : ""
              }`}
            >
              {done.isLoading ? (
                <RowsSkeleton rows={2} />
              ) : doneRows.length === 0 ? (
                <Empty>Nothing finished yet today.</Empty>
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
              count={`${attnItems.length} open · most severe first`}
              action={
                <Link
                  to="/attention"
                  className="inline-flex items-center gap-0.5 text-[11px] text-primary hover:underline"
                >
                  all {attnItems.length} <ArrowUpRight className="h-3 w-3" />
                </Link>
              }
            >
              {attn.isLoading ? (
                <RowsSkeleton rows={2} />
              ) : attnItems.length === 0 ? (
                <Empty>Nothing is waiting on you.</Empty>
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
              count={`${issues.data?.count ?? 0} open${
                degraded > 0 ? ` · ${degraded} repo(s) degraded` : ""
              } · right-click to enqueue`}
            >
              {issues.isLoading ? (
                <RowsSkeleton rows={3} />
              ) : repos.length === 0 ? (
                <Empty>
                  No repos known — set <code className="font-mono">[managed_repos].repos</code>.
                </Empty>
              ) : (
                <div>
                  {repos.map((r) => (
                    <RepoIssueGroup key={r.repo} repo={r} />
                  ))}
                </div>
              )}
              <Link
                to="/repos"
                className="mt-2 inline-flex items-center gap-0.5 text-[11px] text-primary hover:underline"
              >
                all repos <ArrowUpRight className="h-3 w-3" />
              </Link>
            </SectionCard>
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
    </TooltipProvider>
  );
}

export default Home;
