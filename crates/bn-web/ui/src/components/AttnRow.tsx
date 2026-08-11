import { Ban, CalendarClock, Copy, Search, Sparkles } from "lucide-react";

import type { AttnItem } from "../api/types";
import { apiDispatch, attnAction } from "../api/writes";
import { useAppMutation } from "../hooks/useAppMutation";
import { affects } from "../lib/invalidation";
import { dropAttn, dropActionForAttn } from "../lib/optimistic";
import { Hint } from "./SectionCard";
import { SteerButton } from "./SteerButton";
import { Badge } from "./ui/badge";
import { Button } from "./ui/button";
import { Card } from "./ui/card";
import {
  ContextMenu,
  ContextMenuContent,
  ContextMenuItem,
  ContextMenuLabel,
  ContextMenuSeparator,
  ContextMenuTrigger,
} from "./ui/context-menu";

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

/** One attention item — a finding that needs a human. Right-click to investigate, enqueue, snooze
 * or dismiss (all real `/api/attention/:id` verbs); the visible button keeps the common one to hand. */
export function AttnRow({ item }: { item: AttnItem }) {
  // An attention id is not a task id, so this cannot go through the task-agent route — it would
  // resolve no task. It dispatches a scout instead, which is the investigate workflow (dispatch
  // treats `--scout` and `--skill investigate` as the same read-only lane).
  const investigate = useAppMutation<unknown, string>({
    mutationFn: (steer) =>
      apiDispatch({
        repo: item.repo ?? "",
        item: [item.prompt || item.title, item.detail, steer].filter(Boolean).join("\n\n"),
        harness: "claude",
        kind: "scout",
      }),
    invalidates: affects.agentLaunch,
    toast: { success: `Investigating ${item.id} — watch the fleet`, error: "Could not start" },
  });
  // A finding with no repo has nowhere to cut a worktree. Rather than guess one, point at the path
  // that works it out: `plan` files it as a task, and the enhance stage grounds it.
  const investigable = !!item.repo;
  const noRepo = "no repo on this finding — add it to the enhance queue first";
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
            {investigable ? (
              <SteerButton
                label="Investigate"
                icon={Search}
                title="run an investigation agent, steered"
                placeholder="things to look out for (optional)"
                busy={investigate.isPending}
                onSubmit={(steer) => investigate.mutate(steer)}
              />
            ) : (
              // A disabled button fires no pointer events, so the tooltip needs a live wrapper —
              // focusable so the reason is reachable from the keyboard too.
              <Hint label={noRepo}>
                <span tabIndex={0} className="inline-flex rounded-md">
                  <Button size="sm" variant="secondary" disabled className="opacity-50">
                    <Search /> Investigate
                  </Button>
                </span>
              </Hint>
            )}
          </Card>
        </ContextMenuTrigger>
        <ContextMenuContent>
          <ContextMenuLabel>{item.id}</ContextMenuLabel>
          <ContextMenuItem
            disabled={!investigable}
            title={investigable ? undefined : noRepo}
            onSelect={() => investigate.mutate("")}
          >
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
