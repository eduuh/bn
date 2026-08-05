import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { ArrowUpRight, Inbox } from "lucide-react";
import { agentDetailQuery, reviewsQuery } from "../../api/client";
import { pendingReviewFor } from "../../lib/review";
import { fmtTokens, timeAgo } from "../../lib/format";
import { jumpCommand } from "../../lib/board";
import type { ActionItem } from "../../api/types";
import type { WorkItem } from "../../lib/console";
import { AgentActivity } from "../AgentActivity";
import { SteerPanel } from "../SteerPanel";
import { SessionNote } from "../SessionNote";
import { Markdown } from "../Markdown";
import { CopyButton } from "../CopyButton";
import { Badge } from "../ui/badge";
import { cn } from "../../lib/utils";

/** The focus pane's tab strip — a text-underline set, not chunky buttons, so the eye stays on the
 * content below it. */
function Tabs({
  tabs,
  value,
  onChange,
}: {
  tabs: { id: string; label: string }[];
  value: string;
  onChange: (id: string) => void;
}) {
  return (
    <div className="flex items-center gap-4 border-b border-border px-5">
      {tabs.map((t) => (
        <button
          key={t.id}
          type="button"
          onClick={() => onChange(t.id)}
          className={cn(
            "-mb-px border-b-2 py-2 text-[0.78rem] font-medium transition-colors",
            value === t.id
              ? "border-primary text-foreground"
              : "border-transparent text-muted-foreground hover:text-foreground",
          )}
        >
          {t.label}
        </button>
      ))}
    </div>
  );
}

/** The pane header: what you're looking at, in one line, with the tmux jump always to hand. */
function FocusHeader({
  title,
  kind,
  where,
  target,
  children,
}: {
  title: string;
  kind: string;
  where?: string;
  target?: string;
  children?: React.ReactNode;
}) {
  return (
    <header className="flex-none space-y-2 px-5 pb-3 pt-4">
      <div className="flex items-start gap-3">
        <div className="min-w-0 flex-1">
          <div className="mb-1 flex min-w-0 items-center gap-2 text-[0.66rem] font-semibold uppercase tracking-[0.09em] text-muted-foreground">
            <span className="shrink-0">{kind}</span>
            {/* `repo/branch` has no break opportunity a browser will take, so without truncation a
                long dispatch branch stretched this row and wrapped the label one letter per line. */}
            {where && (
              <span className="truncate font-mono normal-case tracking-normal" title={where}>
                {where}
              </span>
            )}
          </div>
          <h2 className="font-display text-[1.35rem] leading-tight tracking-tight text-foreground">
            {title}
          </h2>
        </div>
        {target && <CopyButton text={jumpCommand(target)} label={target} />}
      </div>
      {children}
    </header>
  );
}

/** A live agent in focus: its session activity, its branch note, and the plan it's paused on — all
 * without leaving the queue. */
function AgentFocus({ agent, now }: { agent: NonNullable<WorkItem["agent"]>; now: number }) {
  const [tab, setTab] = useState("live");
  const detail = useQuery({
    ...agentDetailQuery(agent.sessionId ?? ""),
    enabled: !!agent.sessionId,
  });
  const reviews = useQuery(reviewsQuery());
  const review = reviews.data
    ? pendingReviewFor(reviews.data.reviews, agent.repo, agent.branch)
    : undefined;

  const tabs = [
    { id: "live", label: "Live" },
    ...(review ? [{ id: "plan", label: "Plan" }] : []),
    { id: "note", label: "Note" },
  ];

  return (
    <div className="flex h-full min-h-0 flex-col">
      <FocusHeader
        title={agent.stateLabel}
        kind={agent.role === "orchestrator" ? "Orchestrator" : "Agent"}
        where={`${agent.repo}/${agent.branch}`}
        target={agent.target}
      >
        <div className="flex flex-wrap items-center gap-1.5 text-[0.7rem] text-muted-foreground">
          {agent.harness && <Badge variant="muted">{agent.harness}</Badge>}
          {agent.outputTokens != null && (
            <Badge variant="muted">{fmtTokens(agent.outputTokens)} out tok</Badge>
          )}
          <Badge variant="muted">{timeAgo(agent.updatedAt, now)} ago</Badge>
          {agent.needsReason && <Badge variant="warn">{agent.needsReason}</Badge>}
        </div>
      </FocusHeader>

      <Tabs tabs={tabs} value={tab} onChange={setTab} />

      <div className="min-h-0 flex-1 overflow-y-auto p-5">
        {/* Keyed by session, not tmux target: the transcript is per-session and outlives the
            window, so an agent that has already closed its pane still reads back here. */}
        {tab === "live" &&
          (agent.sessionId ? (
            <AgentActivity sessionId={agent.sessionId} />
          ) : (
            <p className="text-sm text-muted-foreground">No session recorded for this entry.</p>
          ))}

        {tab === "plan" && review && (
          <Markdown
            text={review.plan}
            className="rounded-lg border border-border bg-muted/30 p-4"
          />
        )}

        {tab === "note" &&
          (detail.data ? (
            <SessionNote detail={detail.data} />
          ) : (
            <p className="text-sm text-muted-foreground">
              {detail.isLoading ? "Loading the branch note…" : "No branch note for this session."}
            </p>
          ))}
      </div>

      {/* Reply where you read. The composer used to live off in the act column, so answering the
          thing you had just read meant crossing the screen — the agent page never made you do that
          (see `AgentDetail`), and neither should this.

          It sits *outside* the scroll container so it stays pinned while the transcript moves, and
          *outside* the tab switch so a half-typed message survives a trip to the plan or the note —
          the same state-loss the act pane already learned about the hard way. */}
      <div className="flex-none border-t border-border px-5 py-3">
        <SteerPanel target={agent.target} control={agent.control} />
      </div>
    </div>
  );
}

/**
 * A signal in focus. The important change from the old command center: an item's `detail` is
 * **rendered markdown**, not a `<pre>` dump behind a "detail" toggle. Plan bodies, attention
 * write-ups and cron failures are all authored as markdown by the agents that file them, so this
 * is the difference between reading a plan and squinting at one.
 */
function ActionFocus({ item, action }: { item: WorkItem; action: ActionItem }) {
  const reviews = useQuery(reviewsQuery());
  const review =
    action.type === "plan-review" && reviews.data && action.repo && action.branch
      ? pendingReviewFor(reviews.data.reviews, action.repo, action.branch)
      : undefined;
  const body = review?.plan ?? action.detail;

  return (
    <div className="flex h-full min-h-0 flex-col">
      <FocusHeader title={action.title} kind={item.kind} where={item.where}>
        <div className="flex flex-wrap items-center gap-2 text-[0.7rem] text-muted-foreground">
          <span>{action.source}</span>
          {action.link && (
            <a
              href={String(action.link)}
              target="_blank"
              rel="noreferrer"
              className="inline-flex items-center gap-0.5 text-primary"
            >
              open <ArrowUpRight className="h-3 w-3" />
            </a>
          )}
        </div>
      </FocusHeader>

      <div className="min-h-0 flex-1 overflow-y-auto px-5 py-4">
        {body ? (
          <Markdown text={body} className="rounded-lg border border-border bg-muted/30 p-4" />
        ) : (
          <p className="text-sm text-muted-foreground">
            No further detail was recorded for this item — the title is all {action.source}{" "}
            reported.
          </p>
        )}
      </div>
    </div>
  );
}

/** Nothing selected — the reassuring resting state, not an error. */
function EmptyFocus({ blocked }: { blocked: number }) {
  return (
    <div className="flex h-full flex-col items-center justify-center gap-3 px-6 text-center">
      <Inbox className="h-8 w-8 text-muted-foreground/50" />
      <h2 className="font-display text-xl tracking-tight text-foreground">
        {blocked > 0 ? "Pick something up" : "Nothing is blocked on you"}
      </h2>
      <p className="max-w-xs text-sm leading-relaxed text-muted-foreground">
        {blocked > 0
          ? `${blocked} item${blocked === 1 ? " needs" : "s need"} a decision. Press j to walk the list.`
          : "Agents are working. Anything that needs a decision will surface at the top of the queue."}
      </p>
    </div>
  );
}

/** The console's centre pane — whatever is selected, fully resolved, with no navigation. */
export function FocusPane({
  item,
  blocked,
  now,
}: {
  item?: WorkItem;
  blocked: number;
  now: number;
}) {
  if (!item) return <EmptyFocus blocked={blocked} />;
  // Remount per selection so tab state and the activity poll reset cleanly between agents.
  // `agent`/`action` are passed down already-narrowed — this is the only place that knows which
  // kind of row it is, so the panes should not have to re-assert it.
  if (item.agent) return <AgentFocus key={item.id} agent={item.agent} now={now} />;
  if (item.action) return <ActionFocus key={item.id} item={item} action={item.action} />;
  return <EmptyFocus blocked={blocked} />;
}
