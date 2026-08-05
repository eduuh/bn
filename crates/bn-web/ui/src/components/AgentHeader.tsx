import { useState } from "react";
import { ChevronDown } from "lucide-react";
import type { AgentDetailView, HarnessOption } from "../api/types";
import { healthTone, jumpCommand } from "../lib/board";
import { fmtTokens, timeAgo } from "../lib/format";
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
  const outTokens = agent.output_tokens || latest_session?.output_tokens || 0;
  const turns = agent.turns || latest_session?.turns || 0;
  const tone = healthTone(agent.health);

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
        <Tag title="live output tokens">{fmtTokens(outTokens)} out tok</Tag>
        <Tag title="assistant turns">{turns} turns</Tag>
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
        <CopyButton text={jumpCommand(agent.target)} label={`⧉ ${agent.target}`} />
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
