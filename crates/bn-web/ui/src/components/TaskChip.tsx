import { useNavigate } from "react-router-dom";
import { ClipboardList } from "lucide-react";
import type { AgentTask } from "../api/types";
import { cn } from "../lib/utils";

/**
 * The task an agent is working on, as a chip that taps through to it.
 *
 * The fleet could always say an agent was *running*; it could not say what it was running, so a
 * board full of branch names told you nothing about what the machine was actually doing. This is
 * the answer, at the smallest size that still names the work and links to it.
 *
 * A **button driving `useNavigate`, deliberately not a `<Link>`**: both [`FleetRow`] and
 * [`FleetEntryCard`] wrap their entire surface in a `<Link>`, and an anchor nested inside an anchor
 * is invalid HTML — browsers recover from it unpredictably and keyboard navigation breaks outright.
 * Propagation is stopped so the chip wins over the row's own navigation.
 */
export function TaskChip({ task, className }: { task: AgentTask; className?: string }) {
  const navigate = useNavigate();
  return (
    <button
      type="button"
      data-testid="task-chip"
      title={`${task.id} · ${task.summary}${task.stage ? ` · ${task.stage}` : ""}`}
      className={cn(
        "inline-flex min-w-0 items-center gap-1 rounded-md bg-muted px-1.5 py-0.5 text-xs text-muted-foreground transition-colors hover:bg-accent hover:text-foreground",
        className,
      )}
      onClick={(e) => {
        e.preventDefault();
        e.stopPropagation();
        navigate(`/tasks/${encodeURIComponent(task.id)}`);
      }}
    >
      <ClipboardList className="h-3 w-3 shrink-0 opacity-70" aria-hidden="true" />
      <span className="truncate">{task.summary}</span>
    </button>
  );
}

/**
 * The card-only empty state. A card is the surface the board spends on an agent that needs a
 * decision, so "no task" is worth saying there — silently omitting it reads as "there is a task and
 * we didn't render it". Rows stay silent: at row density an extra muted word on every line is
 * noise, not information.
 */
export function NoTaskChip() {
  return (
    <span
      data-testid="no-task"
      title="A task is linked when the agent is dispatched with --task, or with `bn task claim <id>`"
      className="inline-flex items-center gap-1 rounded-md px-1.5 py-0.5 text-xs text-muted-foreground/60"
    >
      no task
    </span>
  );
}
