import { useState } from "react";
import { Link } from "react-router-dom";
import { ChevronDown } from "lucide-react";
import type { AgentDetailView, HarnessOption } from "../api/types";
import { healthTone, jumpCommand } from "../lib/board";
import { fmtCacheReadWrite, fmtTokens, timeAgo } from "../lib/format";
import { CopyButton } from "./CopyButton";
import { HarnessMenu } from "./HarnessMenu";
import { toneVariant } from "../lib/tone";
import { Badge } from "./ui/badge";
import { Card } from "./ui/card";
import { cn } from "../lib/utils";

/** A small metadata tag on the header's stat row. */
function Tag({
  children,
  title,
  className,
}: {
  children: React.ReactNode;
  title?: string;
  className?: string;
}) {
  return (
    <span
      title={title}
      className={cn(
        "inline-flex items-center rounded-md bg-muted px-1.5 py-0.5 text-xs text-muted-foreground",
        className,
      )}
    >
      {children}
    </span>
  );
}

/** A labelled fact in the run-context strip. Values truncate, but keep the full value in `title`. */
function Fact({
  label,
  value,
  title,
  children,
}: {
  label: string;
  value?: string | number | null;
  title?: string;
  children?: React.ReactNode;
}) {
  const body = children ?? (value == null || value === "" ? "—" : value);
  const stringTitle =
    title ?? (typeof body === "string" || typeof body === "number" ? String(body) : undefined);
  return (
    <div className="min-w-0 rounded-md border border-border/70 bg-muted/25 px-2.5 py-2">
      <div className="text-[0.65rem] font-semibold uppercase tracking-wide text-muted-foreground/80">
        {label}
      </div>
      <div className="mt-1 min-w-0 truncate text-sm text-foreground" title={stringTitle}>
        {body}
      </div>
    </div>
  );
}

/** The tone bar colour keyed to the agent's health. */
const BAR: Record<string, string> = {
  green: "bg-ok",
  red: "bg-urgent",
  amber: "bg-warn",
  grey: "bg-border",
  neutral: "bg-border",
};

/**
 * The agent-detail header (design §4(c)): repo/branch, health, the harness switcher, live tokens +
 * turns, the session id, and the tmux target with a copy-jump (#347's web analog). Falls back to
 * the latest recorded session's post-mortem stats when the live session hasn't reported tokens yet.
 *
 * The harness lives on the title row rather than in a card down the sidebar: "what is this agent
 * running under" is identity, read at the same moment as the repo and the branch, and switching it
 * is a property of *this* agent rather than a step in the steer → land column.
 */
export function AgentHeader({
  detail,
  now,
  harnesses,
}: {
  detail: AgentDetailView;
  now: number;
  harnesses?: HarnessOption[];
}) {
  const { agent, latest_session, last_request } = detail;
  const [showRequest, setShowRequest] = useState(false);
  const inTokens = agent.input_tokens || latest_session?.input_tokens || 0;
  const cacheTokens = agent.cache_read_tokens || latest_session?.cache_read_tokens || 0;
  const outTokens = agent.output_tokens || latest_session?.output_tokens || 0;
  // Cache writes are the biggest bucket on a Claude Code run and the dearest per token, so the
  // header shows them next to the reads rather than folding both into one "cache" figure.
  const writeTokens =
    agent.cache_write_tokens ||
    (latest_session
      ? latest_session.cache_write_5m_tokens + latest_session.cache_write_1h_tokens
      : 0);
  const turns = agent.turns || latest_session?.turns || 0;
  const toolCalls = agent.tool_calls || latest_session?.tool_calls || 0;
  const tone = healthTone(agent.health);
  const tmuxTarget = agent.tmux_target ?? agent.target;

  return (
    // Not `overflow-hidden`: the harness menu has to escape the header, so the tone bar rounds
    // its own corners instead of being clipped.
    <Card className="relative p-4 pl-5">
      <span
        className={cn("absolute inset-y-0 left-0 w-1 rounded-l-lg", BAR[tone] ?? BAR.neutral)}
        aria-hidden="true"
      />
      <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
        <h2 className="truncate text-xl font-semibold tracking-tight text-foreground">
          {agent.repo}
        </h2>
        <span className="truncate font-mono text-sm text-muted-foreground">{agent.branch}</span>
        <Badge variant={toneVariant(tone)} className="shrink-0 capitalize">
          {agent.health}
        </Badge>
        {/* Beside the branch, not pushed to the far edge: repo · branch · state · harness is one
            identity cluster, and a `flex-1` spacer would strand it 800px away on a wide header and
            break it onto its own line on a narrow one. */}
        <HarnessMenu target={agent.target} current={agent.harness} harnesses={harnesses} />
      </div>

      <div className="mt-3 flex flex-wrap items-center gap-1.5">
        <Tag title="fresh (uncached) input tokens">{fmtTokens(inTokens)} in</Tag>
        <Tag title="cache reads / cache writes — reads bill ~0.1x fresh input, writes 1.25-2x">
          {fmtCacheReadWrite(cacheTokens, writeTokens)} cache
        </Tag>
        <Tag title="live output tokens">{fmtTokens(outTokens)} out tok</Tag>
        <Tag title="assistant turns">{turns} turns</Tag>
        <Tag title="tool calls">{toolCalls} tools</Tag>
        <Tag title="agent idle time">{Math.round(agent.idle_secs)}s idle</Tag>
        <Tag title="last activity" className="text-muted-foreground/70">
          {timeAgo(agent.last_activity_ts, now)}
        </Tag>
        {latest_session?.outcome && (
          <Tag title="last recorded session outcome">{latest_session.outcome}</Tag>
        )}
      </div>

      <div className="mt-3 flex flex-wrap items-center gap-2">
        <span className="truncate font-mono text-xs text-muted-foreground" title="session id">
          {agent.session_id}
        </span>
        <CopyButton text={jumpCommand(tmuxTarget)} label={`⧉ ${tmuxTarget}`} />
      </div>

      <div className="mt-4 grid grid-cols-1 gap-2 sm:grid-cols-2 xl:grid-cols-4">
        <Fact label="Task">
          {agent.task ? (
            <Link
              to={`/tasks/${agent.task.id}`}
              className="block truncate text-primary hover:underline"
              title={`${agent.task.id} · ${agent.task.summary}`}
            >
              <span className="font-mono">{agent.task.id}</span>
              <span className="text-muted-foreground"> · </span>
              {agent.task.summary}
            </Link>
          ) : (
            <span className="text-muted-foreground">unbound</span>
          )}
        </Fact>
        <Fact label="Worktree" title={agent.worktree}>
          {agent.worktree ? (
            <span className="font-mono text-xs">{agent.worktree}</span>
          ) : (
            <span className="text-muted-foreground">not recorded</span>
          )}
        </Fact>
        <Fact label="Tmux target" title={tmuxTarget}>
          <span className="font-mono text-xs">{tmuxTarget}</span>
        </Fact>
        <Fact label="Control">
          <span className="capitalize">{agent.control ?? "legacy"}</span>
        </Fact>
      </div>

      {last_request && (
        <div className="mt-3">
          <button
            type="button"
            onClick={() => setShowRequest((v) => !v)}
            className="inline-flex items-center gap-1 text-xs font-medium text-primary"
          >
            <ChevronDown
              className={cn("h-3.5 w-3.5 transition-transform", showRequest && "rotate-180")}
            />
            {showRequest ? "hide last request" : "last request"}
          </button>
          {showRequest && (
            <pre className="mt-2 max-h-64 overflow-auto rounded-md border border-border bg-muted/60 p-3 font-mono text-xs text-muted-foreground">
              {last_request}
            </pre>
          )}
        </div>
      )}
    </Card>
  );
}
