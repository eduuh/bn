import { useEffect, useRef, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { AlertTriangle, ChevronRight, Terminal, Wrench } from "lucide-react";
import type { ActivityEntry, ToolCall } from "../api/types";
import { activityQuery } from "../api/client";
import { useNow } from "../hooks/useNow";
import { timeAgo } from "../lib/format";
import { Markdown } from "./Markdown";
import { Skeleton } from "./ui/skeleton";
import { cn } from "../lib/utils";

/** How many trailing entries to request. Deep enough to hold the current stretch of work, shallow
 * enough that folding a megabyte transcript stays cheap on every 4s tick. */
const LIMIT = 40;

/** A collapsed run of tool calls. Closed it's one dim line ("14 tool calls · Read ×6, Bash ×3");
 * open it lists each call's target. Collapsed by default because the prose either side of a tool
 * run is what you're actually reading — the calls are there when you want to know *how*. */
function ToolRun({ summary, tools }: { summary: string; tools: ToolCall[] }) {
  const [open, setOpen] = useState(false);
  return (
    <div className="py-1">
      <button
        type="button"
        onClick={() => setOpen((o) => !o)}
        aria-expanded={open}
        className={cn(
          "group flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left",
          "text-xs text-muted-foreground transition-colors hover:bg-muted/50 hover:text-foreground",
        )}
      >
        <ChevronRight
          className={cn("h-3.5 w-3.5 shrink-0 transition-transform", open && "rotate-90")}
          aria-hidden="true"
        />
        <Wrench className="h-3.5 w-3.5 shrink-0 opacity-60" aria-hidden="true" />
        <span className="truncate">{summary}</span>
      </button>

      {open && (
        <ul className="ml-[1.6rem] space-y-1 border-l border-border/60 py-1.5 pl-3">
          {tools.map((t, i) => (
            <li key={i} className="flex items-baseline gap-2 text-xs">
              <span className="shrink-0 font-medium text-foreground/70">{t.name}</span>
              {t.target && (
                <span className="truncate font-mono text-[0.7rem] text-muted-foreground">
                  {t.target}
                </span>
              )}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

/** A prose turn — yours or the agent's. The speaker is a small caps label rather than a chat
 * bubble: this is a log you scan top-to-bottom, and bubbles would halve the usable width on a
 * phone. A user turn gets a left rule in the accent so your own prompts are findable in a long
 * scroll. */
function Turn({ entry, now }: { entry: ActivityEntry; now: number }) {
  const mine = entry.kind === "user";
  return (
    <article className={cn("py-3", mine && "border-l-2 border-primary/60 pl-3")}>
      <header className="mb-1.5 flex items-baseline gap-2">
        <span
          className={cn(
            "text-[0.7rem] font-semibold uppercase tracking-wide",
            mine ? "text-primary" : "text-muted-foreground",
          )}
        >
          {mine ? "you" : "agent"}
        </span>
        {entry.ts > 0 && (
          <time className="text-[0.7rem] text-muted-foreground/70">{timeAgo(entry.ts, now)}</time>
        )}
      </header>
      <Markdown text={entry.text} />
    </article>
  );
}

/**
 * The agent's session timeline — the supervise surface that replaced the xterm pane mirror.
 *
 * The old view streamed `tmux capture-pane` into xterm.js. It could never work: the agent's real
 * pane is ~200 columns, the browser grid is roughly half that, so every line wrapped and the TUI's
 * box rules doubled into noise — and an agent whose window had already closed showed nothing at
 * all. This reads the harness transcript instead (`GET /api/agents/:id/activity`), so the content
 * is prose that reflows to whatever width you're on, and a closed window is no longer the end of
 * the story.
 *
 * Rows are your prompts, the agent's messages as real markdown, and consecutive tool calls folded
 * into one expandable summary. The feed pins to the bottom (newest) unless you've scrolled up to
 * read — then it holds position rather than yanking you back on the next 4s tick.
 */
export function AgentActivity({ sessionId }: { sessionId: string }) {
  const activity = useQuery(activityQuery(sessionId, LIMIT));
  const now = useNow();
  const scrollRef = useRef<HTMLDivElement>(null);
  // Whether the viewport is parked at the newest entry. Starts true so the first render lands at
  // the bottom; flipped by the scroll handler as soon as you move away.
  const pinnedRef = useRef(true);

  const entries = activity.data?.entries ?? [];
  const lastTs = entries.length > 0 ? entries[entries.length - 1].ts : 0;

  // Follow new entries only while pinned. Keyed on the newest timestamp *and* the count, so a fresh
  // entry scrolls but an unchanged refetch doesn't.
  useEffect(() => {
    const el = scrollRef.current;
    if (!el || !pinnedRef.current) return;
    el.scrollTop = el.scrollHeight;
  }, [lastTs, entries.length]);

  const onScroll = () => {
    const el = scrollRef.current;
    if (!el) return;
    // A small slack so "close enough to the bottom" still counts as following.
    pinnedRef.current = el.scrollHeight - el.scrollTop - el.clientHeight < 48;
  };

  const total = activity.data?.total ?? 0;
  const hidden = Math.max(0, total - entries.length);

  return (
    <section className="flex flex-col overflow-hidden rounded-lg border border-border bg-card shadow-card">
      <div className="flex flex-wrap items-center gap-2 border-b border-border bg-muted/40 px-3 py-2">
        <h3 className="text-sm font-semibold text-foreground">Activity</h3>
        {activity.data && (
          <span className="font-mono text-xs text-muted-foreground">
            {activity.data.harness}
            {hidden > 0 && ` · last ${entries.length} of ${total}`}
          </span>
        )}
        <span className="flex-1" />
        {activity.isFetching && (
          <span className="text-xs text-muted-foreground" aria-live="polite">
            syncing…
          </span>
        )}
      </div>

      <div
        ref={scrollRef}
        onScroll={onScroll}
        data-testid="activity-scroll"
        // Viewport-relative so the steer composer directly below stays reachable without a page
        // scroll on a laptop, while a tall screen still gets a generous feed.
        className="max-h-[clamp(16rem,52vh,32rem)] min-h-[16rem] divide-y divide-border/50 overflow-y-auto px-3"
      >
        {activity.isLoading && (
          <div className="space-y-3 py-4" aria-hidden="true">
            <Skeleton className="h-4 w-1/3" />
            <Skeleton className="h-16 w-full" />
            <Skeleton className="h-4 w-1/4" />
            <Skeleton className="h-24 w-full" />
          </div>
        )}

        {activity.isError && (
          <div
            data-testid="activity-error"
            className="flex flex-col items-center gap-2 py-10 text-center"
          >
            <AlertTriangle className="h-6 w-6 text-muted-foreground" />
            <p className="text-sm font-medium text-foreground">No transcript for this session</p>
            <p className="max-w-xs text-xs text-muted-foreground">
              The harness hasn't written one yet, or it's been cleaned up. The agent may still be
              running — check the pane in tmux.
            </p>
          </div>
        )}

        {activity.data && entries.length === 0 && (
          <div className="flex flex-col items-center gap-2 py-10 text-center">
            <Terminal className="h-6 w-6 text-muted-foreground" />
            <p className="text-sm text-muted-foreground">
              Nothing in this session yet — it's just started.
            </p>
          </div>
        )}

        {entries.map((e, i) =>
          e.kind === "tools" ? (
            <ToolRun key={`${e.ts}-${i}`} summary={e.summary} tools={e.tools} />
          ) : (
            <Turn key={`${e.ts}-${i}`} entry={e} now={now} />
          ),
        )}
      </div>
    </section>
  );
}
