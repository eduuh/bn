import { type ReactNode, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import {
  ArrowRight,
  Check,
  ChevronRight,
  ExternalLink,
  MessageSquareText,
  NotebookText,
} from "lucide-react";
import { Link } from "react-router-dom";

import {
  completedQuery,
  fleetQuery,
  orchestrateQuery,
  queueQuery,
  reviewsQuery,
} from "../api/client";
import { decideReview, sendMessage } from "../api/writes";
import { entryTitle } from "../api/queue-types";
import type { QueueEntryView, QueueNodeRow, QueueView } from "../api/queue-types";
import { isRunning, runningCount } from "../api/types";
import type { CompletedItem, FleetView, LiveAgent, Review } from "../api/types";
import { QueueControls } from "../components/QueueControls";
import { QueueReservoir } from "../components/QueueReservoir";
import { QueueUpcoming } from "../components/QueueUpcoming";
import { QueueVerdict } from "../components/QueueVerdict";
import { QueueWorkLedger } from "../components/QueueWorkLedger";
import { RecoveryFeed } from "../components/RecoveryFeed";
import { RunControls } from "../components/RunControls";
import { Empty, LoadError, RowsSkeleton, SectionCard } from "../components/SectionCard";
import { SteerButton } from "../components/SteerButton";
import { ToastStack } from "../components/ToastStack";
import { Badge, type BadgeProps } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import { useAppMutation } from "../hooks/useAppMutation";
import { affects } from "../lib/invalidation";
import { setReviewStatus } from "../lib/optimistic";
import { sanitizeBranch } from "../lib/board";
import { timeAgo } from "../lib/format";
import { cn } from "../lib/utils";

const HEALTH_TONE: Record<string, BadgeProps["variant"]> = {
  active: "ok",
  starting: "muted",
  idle: "warn",
  stuck: "urgent",
  ended: "muted",
};

const NODE_TONE: Record<string, BadgeProps["variant"]> = {
  ready: "ok",
  draining: "warn",
  offline: "urgent",
  unknown: "muted",
};

const KIND_TONE: Record<string, string> = {
  ship: "text-primary",
  enhance: "text-ok",
  "review-pr": "text-urgent",
  investigate: "text-info",
  research: "text-warn",
  scout: "text-warn",
  cron: "text-muted-foreground",
  cleanup: "text-muted-foreground",
};

function sameWorktree(review: Review, agent: LiveAgent): boolean {
  return (
    review.repo.toLowerCase() === agent.repo.toLowerCase() &&
    sanitizeBranch(review.branch) === sanitizeBranch(agent.branch)
  );
}

function noteHref(repo: string, branch: string): string {
  return `/notes?repo=${encodeURIComponent(repo)}&branch=${encodeURIComponent(branch)}`;
}

function Metric({
  label,
  value,
  detail,
  tone,
}: {
  label: string;
  value: ReactNode;
  detail?: ReactNode;
  tone?: string;
}) {
  return (
    <div className="min-w-0 bg-card px-3 py-2.5">
      <p className="font-mono text-[10px] uppercase tracking-[0.12em] text-muted-foreground">
        {label}
      </p>
      <p className={cn("mt-1 text-lg font-semibold tabular-nums", tone)}>{value}</p>
      {detail ? <p className="truncate text-[11px] text-muted-foreground">{detail}</p> : null}
    </div>
  );
}

function OperationsOverview({
  data,
  fleet,
  error,
}: {
  data?: QueueView;
  fleet?: FleetView;
  error: boolean;
}) {
  if (error) {
    return (
      <SectionCard eyebrow="live system" title="Operations">
        <LoadError>Could not load the machine queue.</LoadError>
      </SectionCard>
    );
  }
  if (!data) {
    return (
      <SectionCard eyebrow="live system" title="Operations">
        <RowsSkeleton rows={2} />
      </SectionCard>
    );
  }

  const slots = data.nodes.reduce((sum, node) => sum + node.slots, 0);
  const free = data.nodes.reduce((sum, node) => sum + node.free, 0);
  const machine = data.machine;
  const lanes = machine?.lanes ?? [];

  return (
    <SectionCard
      eyebrow="live system"
      title="Operations"
      count={`updated ${new Date(data.now * 1000).toLocaleTimeString()}`}
    >
      <div className="grid overflow-hidden rounded-md border border-border bg-border sm:grid-cols-2 lg:grid-cols-5">
        <Metric
          label="dispatcher"
          value={machine?.dispatcher_alive ? "online" : "offline"}
          detail={data.accepting ? "accepting work" : "not accepting work"}
          tone={machine?.dispatcher_alive ? "text-ok" : "text-urgent"}
        />
        <Metric
          label="live agents"
          // Running only, so this tile and the "Running" card above it cannot disagree about how
          // busy the machine is — `agents.length` also counts the sessions that have finished.
          value={fleet ? runningCount(fleet) : "—"}
          detail={
            fleet
              ? `${fleet.active} active · ${fleet.idle} idle · ${fleet.stuck} stuck`
              : "fleet loading"
          }
          tone={(fleet?.stuck ?? 0) > 0 ? "text-urgent" : undefined}
        />
        <Metric
          label="worker slots"
          value={slots > 0 ? `${free}/${slots}` : "—"}
          detail="free across registered nodes"
          tone={free > 0 ? "text-ok" : undefined}
        />
        <Metric
          label="backlog"
          value={data.pending}
          detail="admitted entries"
          tone={data.pending > 0 ? "text-info" : undefined}
        />
        <Metric
          label="reservoir"
          value={data.reservoir?.total ?? "—"}
          detail={data.reservoir ? `${data.reservoir.ready} ready to admit` : "store unavailable"}
          tone={(data.reservoir?.stuck ?? 0) > 0 ? "text-warn" : undefined}
        />
      </div>

      {lanes.length > 0 ? (
        <div className="mt-3 space-y-2">
          <div className="flex items-baseline justify-between gap-3">
            <p className="text-xs font-medium">Global queue lanes</p>
            <p className="text-[11px] text-muted-foreground">{machine?.lanes_why}</p>
          </div>
          {lanes.map((lane) => {
            const pct = lane.cap > 0 ? Math.min(100, (lane.used / lane.cap) * 100) : 0;
            return (
              <div key={lane.lane} className="flex items-center gap-3">
                <span className="w-20 font-mono text-xs text-muted-foreground">{lane.lane}</span>
                <div className="h-2 flex-1 overflow-hidden rounded-full bg-muted">
                  <div
                    className={cn(
                      "h-full rounded-full",
                      lane.used >= lane.cap ? "bg-warn" : "bg-primary",
                    )}
                    style={{ width: `${pct}%` }}
                  />
                </div>
                <span className="w-12 text-right font-mono text-xs tabular-nums">
                  {lane.used}/{lane.cap}
                </span>
              </div>
            );
          })}
          <div className="flex flex-wrap items-center gap-2 pt-1 text-[11px] text-muted-foreground">
            <span>
              Build pool {machine?.builds.held.length ?? 0}/{machine?.builds.cap ?? 0}
            </span>
            {(machine?.captured ?? []).map((producer) => (
              <Badge key={producer} variant="muted">
                {producer}
              </Badge>
            ))}
          </div>
        </div>
      ) : null}

      {data.decision ? (
        <div className="mt-3 flex flex-wrap items-baseline gap-x-2 gap-y-1 border-t border-border pt-3 text-sm">
          <span className="font-medium">Last decision</span>
          <Badge variant={data.decision.started > 0 ? "ok" : "muted"}>
            {data.decision.outcome}
          </Badge>
          <span className="text-muted-foreground">
            {data.decision.started} started · {data.decision.deferred.length} held
          </span>
          {data.decision.reason ? (
            <span className="text-foreground/80">— {data.decision.reason}</span>
          ) : null}
        </div>
      ) : null}
    </SectionCard>
  );
}

function AgentActions({ agent }: { agent: LiveAgent }) {
  const steer = useAppMutation<unknown, string>({
    mutationFn: (message) => sendMessage(agent.target, message),
    invalidates: affects.agentSteer,
    toast: { success: `Steered ${agent.target}`, error: "Could not steer agent" },
  });
  const steerable = agent.control !== "none" && agent.control !== "ambiguous";

  return (
    <div className="flex shrink-0 flex-wrap items-center justify-end gap-1">
      <Button asChild size="sm" variant="secondary">
        <Link to={`/agents/${encodeURIComponent(agent.session_id)}`}>Open agent</Link>
      </Button>
      <Button asChild size="sm" variant="ghost">
        <Link to={noteHref(agent.repo, agent.branch)}>
          <NotebookText />
          Note
        </Link>
      </Button>
      {steerable ? (
        <SteerButton
          label="Steer"
          title={`Send guidance to ${agent.target}`}
          placeholder="What should this agent change or prioritize?"
          icon={MessageSquareText}
          variant="ghost"
          busy={steer.isPending}
          required
          help="Guidance is sent through the agent's live control channel."
          onSubmit={(message) => steer.mutate(message)}
        />
      ) : null}
    </div>
  );
}

function AgentRow({ agent, now }: { agent: LiveAgent; now: number }) {
  return (
    <li className="flex flex-wrap items-center gap-3 py-3">
      <Badge variant={HEALTH_TONE[agent.health] ?? "muted"} className="capitalize">
        {agent.health}
      </Badge>
      <div className="min-w-64 flex-1">
        <p className="truncate text-sm font-medium">
          {agent.task?.summary ?? `${agent.repo} / ${agent.branch}`}
        </p>
        <div className="flex flex-wrap items-center gap-x-2 text-[11px] text-muted-foreground">
          <span className="font-mono">{agent.target}</span>
          <span>{agent.harness}</span>
          <span>active {timeAgo(agent.last_activity_ts, now)} ago</span>
          {agent.role === "orchestrator" ? <span>orchestrator</span> : null}
        </div>
      </div>
      {agent.task ? (
        <Button asChild size="sm" variant="ghost">
          <Link to={`/tasks/${encodeURIComponent(agent.task.id)}`}>{agent.task.id}</Link>
        </Button>
      ) : null}
      <AgentActions agent={agent} />
    </li>
  );
}

/** One leased non-agent entry — a cron firing, a heavy job building a pool, a shell step. These
 *  have no session to open and nothing to steer; what they owe you is what they are and how long
 *  they have been at it. */
function RunningJobRow({ view, now }: { view: QueueEntryView; now: number }) {
  const { entry } = view;
  const exec = entry.exec ?? {};
  // The job identifier is the precise name for cron/shell work; `entryTitle` is the shared
  // fallback, and it exists because a raw `item` is the entry's whole seed prompt.
  const what = exec.job ?? exec.skill ?? exec.cmd ?? entryTitle(entry) ?? entry.id;
  return (
    <li className="flex flex-wrap items-baseline gap-x-2 gap-y-1 py-2.5">
      <Badge variant="info">{entry.kind}</Badge>
      <span className="min-w-0 flex-1 truncate text-sm font-medium" title={what}>
        {what}
      </span>
      <span className="font-mono text-[11px] text-muted-foreground">
        {entry.repo}
        {view.class.subtype !== "foreground" ? ` · ${view.class.subtype}` : ""}
      </span>
      <span className="font-mono text-[11px] tabular-nums text-muted-foreground">
        running {timeAgo(entry.updated_at, now)}
      </span>
    </li>
  );
}

/**
 * Everything running right now, agents and machinery together.
 *
 * The card used to be "Working agents" and showed only sessions, which meant a machine busy firing
 * crons and warming pools rendered as idle — the queue had leased the work and started it, and the
 * one screen asking "what is happening" left it out. `queue.active` carried those rows the whole
 * time and nothing displayed them.
 *
 * Agent-backed entries are filtered out of the job list by the caller rather than shown twice: a
 * dispatched agent is already the row above, and the same work counted in two places is worse than
 * either count alone.
 *
 * `agents` is what the caller has already filtered down to: the running sessions, minus the ones
 * `NeedsInput` is rendering as their own rows above. Both exclusions matter — a stuck agent listed
 * here as well was the same agent twice on one screen, and a finished session listed at all was the
 * page answering "what is running" with work that is over.
 */
function ActiveNow({
  agents,
  jobs,
  now,
  loading,
  error,
}: {
  agents: LiveAgent[];
  jobs: QueueEntryView[];
  now: number;
  loading: boolean;
  error: boolean;
}) {
  const total = agents.length + jobs.length;
  const parts = [
    agents.length > 0 ? `${agents.length} agent${agents.length === 1 ? "" : "s"}` : null,
    jobs.length > 0 ? `${jobs.length} job${jobs.length === 1 ? "" : "s"}` : null,
  ].filter(Boolean);

  return (
    <SectionCard
      eyebrow="active now"
      title="Running"
      count={parts.length > 0 ? parts.join(" · ") : String(total)}
    >
      {error ? (
        <LoadError>Could not load the live fleet.</LoadError>
      ) : loading ? (
        <RowsSkeleton rows={3} />
      ) : total === 0 ? (
        <Empty>Nothing is running right now.</Empty>
      ) : (
        <ul className="divide-y divide-border">
          {agents.map((agent) => (
            <AgentRow key={agent.session_id} agent={agent} now={now} />
          ))}
          {jobs.map((view) => (
            <RunningJobRow key={`${view.entry.repo}/${view.entry.id}`} view={view} now={now} />
          ))}
        </ul>
      )}
    </SectionCard>
  );
}

function reviewTitle(review: Review): string {
  return (
    review.plan
      .split("\n")
      .map((line) => line.replace(/^#+\s*/, "").trim())
      .find(Boolean) ?? `${review.repo} plan`
  );
}

function ReviewRow({ review, agent }: { review: Review; agent?: LiveAgent }) {
  const approve = useAppMutation({
    mutationFn: () => decideReview(review.id, "approve"),
    optimistic: setReviewStatus(review.id, "approved"),
    invalidates: affects.review,
    toast: { success: "Plan approved", error: "Could not approve plan" },
  });
  const changes = useAppMutation<unknown, string>({
    mutationFn: (notes) => decideReview(review.id, "request_changes", notes),
    optimistic: setReviewStatus(review.id, "changes-requested"),
    invalidates: affects.review,
    toast: { success: "Guidance sent", error: "Could not request changes" },
  });
  const sessionId = agent?.session_id ?? review.session;
  const busy = approve.isPending || changes.isPending;

  return (
    <li className="flex flex-wrap items-center gap-3 py-3">
      <Badge variant="warn">plan review</Badge>
      <div className="min-w-64 flex-1">
        <p className="truncate text-sm font-medium" title={reviewTitle(review)}>
          {reviewTitle(review)}
        </p>
        <p className="font-mono text-[11px] text-muted-foreground">
          {review.repo}/{review.branch}
        </p>
      </div>
      <div className="flex shrink-0 flex-wrap items-center justify-end gap-1">
        <Button size="sm" disabled={busy} onClick={() => approve.mutate()}>
          <Check />
          Approve
        </Button>
        <SteerButton
          label="Steer"
          title="Request changes and send the agent specific guidance"
          placeholder="What must change before this plan can run?"
          icon={MessageSquareText}
          busy={busy}
          required
          help="A concrete note is required and is delivered back to the waiting agent."
          onSubmit={(notes) => changes.mutate(notes)}
        />
        {sessionId ? (
          <Button asChild size="sm" variant="secondary">
            <Link to={`/agents/${encodeURIComponent(sessionId)}`}>Agent</Link>
          </Button>
        ) : null}
        <Button asChild size="sm" variant="ghost">
          <Link to={noteHref(review.repo, review.branch)}>
            <NotebookText />
            Note
          </Link>
        </Button>
      </div>
    </li>
  );
}

function NeedsInput({
  reviews,
  agents,
  loading,
  error,
  now,
}: {
  reviews: Review[];
  agents: LiveAgent[];
  loading: boolean;
  error: boolean;
  now: number;
}) {
  const pending = reviews.filter((review) => review.status === "pending");
  const stuck = agents.filter(
    (agent) => agent.health === "stuck" && !pending.some((review) => sameWorktree(review, agent)),
  );
  const count = pending.length + stuck.length;

  return (
    <SectionCard
      eyebrow="needs you"
      title="Decisions and guidance"
      count={String(count)}
      tone={count > 0 ? "ready" : undefined}
    >
      {error ? (
        <LoadError>Could not load pending decisions.</LoadError>
      ) : loading ? (
        <RowsSkeleton rows={2} />
      ) : count === 0 ? (
        <Empty>No approvals or stuck agents need you.</Empty>
      ) : (
        <ul className="divide-y divide-border">
          {pending.map((review) => (
            <ReviewRow
              key={review.id}
              review={review}
              agent={agents.find((agent) => sameWorktree(review, agent))}
            />
          ))}
          {stuck.map((agent) => (
            <AgentRow key={`stuck:${agent.session_id}`} agent={agent} now={now} />
          ))}
        </ul>
      )}
    </SectionCard>
  );
}

function formatMb(value: number | null | undefined): string {
  if (value == null) return "—";
  if (value >= 1024) return `${(value / 1024).toFixed(value >= 10_240 ? 0 : 1)} GB`;
  return `${value} MB`;
}

function NodeRow({ node }: { node: QueueNodeRow }) {
  const capacity = node.capacity;
  const heartbeat =
    node.heartbeat_age_secs == null
      ? "never heartbeated"
      : node.heartbeat_age_secs < 60
        ? `${node.heartbeat_age_secs}s ago`
        : `${Math.floor(node.heartbeat_age_secs / 60)}m ago`;
  const totalMemory = capacity?.mem_total_mb ?? (node.ram_total_mb || null);

  return (
    <li className="space-y-2 py-3">
      <div className="flex flex-wrap items-center gap-2">
        <span className="font-medium">{node.node_id}</span>
        <Badge variant={NODE_TONE[node.status] ?? "muted"}>{node.status}</Badge>
        {node.stale ? <Badge variant="urgent">stale heartbeat</Badge> : null}
        <span className="font-mono text-xs text-muted-foreground">{node.target}</span>
        <span className="ml-auto font-mono text-xs tabular-nums text-muted-foreground">
          {node.free}/{node.slots} slots free
        </span>
      </div>
      <div className="flex flex-wrap gap-x-5 gap-y-1 text-xs text-muted-foreground">
        <span>heartbeat {heartbeat}</span>
        <span>
          memory {formatMb(capacity?.mem_available_mb)} free / {formatMb(totalMemory)}
        </span>
        <span>
          load {capacity?.loadavg1?.toFixed(2) ?? "—"} / {capacity?.nproc ?? "—"} cores
        </span>
        <span>disk {formatMb(capacity?.disk_avail_mb)} free</span>
      </div>
      <div className="flex flex-wrap items-center gap-1">
        {node.harnesses.map((harness) => (
          <Badge key={`harness:${harness}`} variant="info">
            {harness}
          </Badge>
        ))}
        {node.skills.map((skill) => (
          <Badge key={`skill:${skill}`} variant="secondary">
            skill:{skill}
          </Badge>
        ))}
        {node.labels
          .filter((label) => !label.startsWith("skill:") && !label.startsWith("harness:"))
          .slice(0, 8)
          .map((label) => (
            <Badge key={label} variant="muted">
              {label}
            </Badge>
          ))}
        {node.owned_repos.length > 0 ? (
          <span className="ml-1 text-[11px] text-muted-foreground">
            owns {node.owned_repos.join(", ")}
          </span>
        ) : null}
      </div>
    </li>
  );
}

function ClusterCapacity({
  nodes,
  loading,
  error,
}: {
  nodes: QueueNodeRow[];
  loading: boolean;
  error: boolean;
}) {
  const slots = nodes.reduce((sum, node) => sum + node.slots, 0);
  const free = nodes.reduce((sum, node) => sum + node.free, 0);
  return (
    <SectionCard
      eyebrow="cluster"
      title="Node capacity"
      count={`${free}/${slots} worker slots free`}
    >
      {error ? (
        <LoadError>Could not load cluster capacity.</LoadError>
      ) : loading ? (
        <RowsSkeleton rows={2} />
      ) : nodes.length === 0 ? (
        <Empty>No worker nodes are registered.</Empty>
      ) : (
        <ul className="divide-y divide-border">
          {nodes.map((node) => (
            <NodeRow key={node.node_id} node={node} />
          ))}
        </ul>
      )}
    </SectionCard>
  );
}

function OutcomeLink({ item }: { item: CompletedItem }) {
  if (item.repo && item.branch) {
    return (
      <Button asChild size="sm" variant="ghost">
        <Link to={noteHref(item.repo, item.branch)}>
          <NotebookText />
          Note
        </Link>
      </Button>
    );
  }
  if (item.note.remote_url) {
    return (
      <Button asChild size="sm" variant="ghost">
        <a href={item.note.remote_url} target="_blank" rel="noreferrer">
          <NotebookText />
          Note
        </a>
      </Button>
    );
  }
  return null;
}

function RecentOutcomes({
  rows,
  now,
  loading,
  error,
}: {
  rows: CompletedItem[];
  now: number;
  loading: boolean;
  error: boolean;
}) {
  const shown = rows.slice(0, 10);
  return (
    <SectionCard eyebrow="durable record" title="Recent outcomes" count={String(rows.length)}>
      {error ? (
        <LoadError>Could not load completed outcomes.</LoadError>
      ) : loading ? (
        <RowsSkeleton rows={3} />
      ) : shown.length === 0 ? (
        <Empty>No completed outcomes have been recorded yet.</Empty>
      ) : (
        <ul className="divide-y divide-border">
          {shown.map((item) => (
            <li key={item.id} className="flex flex-wrap items-center gap-3 py-3">
              <Badge variant="ok">{item.type}</Badge>
              <div className="min-w-64 flex-1">
                <p className="text-sm font-medium">{item.title}</p>
                <p className="text-[11px] text-muted-foreground">
                  {[item.repo, item.branch, item.detail].filter(Boolean).join(" · ")}
                </p>
              </div>
              <span className="font-mono text-[11px] text-muted-foreground">
                {timeAgo(item.completed, now)} ago
              </span>
              {item.link ? (
                <Button asChild size="sm" variant="ghost">
                  <a href={item.link} target="_blank" rel="noreferrer">
                    <ExternalLink />
                    Artifact
                  </a>
                </Button>
              ) : null}
              <OutcomeLink item={item} />
            </li>
          ))}
        </ul>
      )}
      {rows.length > shown.length ? (
        <p className="mt-2 text-[11px] text-muted-foreground">
          Showing the 10 most recent of {rows.length}.
        </p>
      ) : null}
    </SectionCard>
  );
}

function QueueShape({ data }: { data?: QueueView }) {
  const kinds = (data?.kinds ?? []).filter((kind) => kind.pending > 0);
  if (kinds.length === 0) return null;
  return (
    <SectionCard eyebrow="waiting" title="Queued by kind" count={String(data?.pending ?? 0)}>
      <div className="flex flex-wrap gap-x-6 gap-y-2">
        {kinds.map((kind) => (
          <div key={kind.kind} className="flex items-baseline gap-2 text-sm">
            <span className={cn("font-mono text-xs", KIND_TONE[kind.kind])}>{kind.kind}</span>
            <span className="font-mono tabular-nums">{kind.pending}</span>
            <span className="text-xs text-muted-foreground">{kind.lane}</span>
          </div>
        ))}
      </div>
    </SectionCard>
  );
}

/**
 * The detail shelf: the sections that answer a question you went looking for, rather than one the
 * page should volunteer.
 *
 * Their children each fetch their own data, so they are mounted only once opened — closed, the
 * shelf costs nothing, and the page stops issuing five queries whose results nobody asked to see.
 */
function DetailShelf({ children, summary }: { children: ReactNode; summary: string }) {
  const [open, setOpen] = useState(false);
  return (
    <div className="space-y-4">
      <button
        type="button"
        onClick={() => setOpen((prev) => !prev)}
        aria-expanded={open}
        className="flex w-full items-center gap-2 rounded-lg border border-border bg-card px-3.5 py-2.5 text-left transition hover:bg-muted/40"
      >
        <ChevronRight
          className={cn("size-3.5 shrink-0 transition-transform", open && "rotate-90")}
        />
        <span className="text-sm font-medium">Detail</span>
        <span className="truncate text-[11px] text-muted-foreground">{summary}</span>
      </button>
      {open ? children : null}
    </div>
  );
}

export function Queue() {
  const queue = useQuery(queueQuery());
  const fleet = useQuery(fleetQuery());
  const reviews = useQuery(reviewsQuery());
  const completed = useQuery(completedQuery());
  const data = queue.data;
  // The fleet reports every session whose transcript is still readable, which outlasts the session
  // itself by half an hour. This page asks "what is running", so it wants only the ones that are.
  const agents = (fleet.data?.agents ?? []).filter(isRunning);
  const now = data?.now ?? Math.floor(Date.now() / 1000);

  // The same two counts `NeedsInput` derives, hoisted so the page can decide whether the card is
  // worth a band at all. A section that only ever says "nothing needs you" is page furniture.
  const pendingReviews = (reviews.data?.reviews ?? []).filter((r) => r.status === "pending");
  const stuckAgents = agents.filter(
    (agent) =>
      agent.health === "stuck" && !pendingReviews.some((review) => sameWorktree(review, agent)),
  );
  const needsYou = pendingReviews.length + stuckAgents.length;
  // Anything "Decisions and guidance" is already rendering does not appear under "Running" as well —
  // a stuck agent as its own row there, an agent whose plan is waiting as that review's row. Both
  // were the same agent twice on one screen, which is the double-count the job list refuses one line
  // down, and an agent waiting on a person is not running in any case.
  const promoted = new Set(
    agents
      .filter(
        (agent) =>
          agent.health === "stuck" || pendingReviews.some((review) => sameWorktree(review, agent)),
      )
      .map((agent) => agent.session_id),
  );
  const working = agents.filter((agent) => !promoted.has(agent.session_id));
  const outcomes = completed.data?.completed ?? [];
  const nodes = data?.nodes ?? [];

  // Leased work that is not an agent: cron firings, pool warms, heavy shell steps. Agent-backed
  // entries are dropped because the fleet already lists each of them as a session.
  const runningJobs = (data?.active ?? []).filter((view) => view.entry.exec?.run !== "agent");

  // Only the queue query gates the first paint. The others fill their own bands in, and holding
  // the whole page for the slowest of four is what made a live machine look empty on arrival.
  const settling = queue.isLoading;
  const notEnabled = data?.state === "not_enabled";

  return (
    <div className="mx-auto w-full max-w-6xl space-y-4 px-4 py-5">
      <header className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <h1 className="text-xl font-semibold">Queue</h1>
          <p className="text-sm text-muted-foreground">What is running, what is held, and why.</p>
        </div>
        <div className="flex flex-wrap items-center justify-end gap-2">
          <Button asChild size="sm" variant="secondary">
            <Link to="/queue/events">Queue events</Link>
          </Button>
          <Button asChild size="sm" variant="secondary">
            <Link to="/today">
              Plan work
              <ArrowRight />
            </Link>
          </Button>
        </div>
      </header>

      {/* The verdict leads: on a stalled machine every card below it reports a number, and none of
          them says why the numbers are not moving. */}
      <QueueVerdict data={data} loading={queue.isLoading} error={queue.isError} />

      {notEnabled ? null : (
        <>
          {/* Below the verdict the page shows only what is true right now. Each of these used to render
          a card reading "nothing", so a quiet machine produced a full screen of scrolling that said
          nothing at all — and a busy one buried the two bands that mattered among the ten that did
          not. A band that would be empty is not rendered; its absence is the same information. */}
          {needsYou > 0 ? (
            <NeedsInput
              reviews={reviews.data?.reviews ?? []}
              agents={agents}
              now={now}
              loading={false}
              error={reviews.isError || fleet.isError}
            />
          ) : null}

          {working.length > 0 || runningJobs.length > 0 || fleet.isError ? (
            <ActiveNow
              agents={working}
              jobs={runningJobs}
              now={now}
              loading={false}
              error={fleet.isError}
            />
          ) : null}

          {/* Already self-hiding: each renders null with nothing to show. */}
          <RunSteering />
          <QueueShape data={data} />
          <RecoveryFeed />

          {settling ? <RowsSkeleton rows={2} /> : null}

          {/* Outcomes are history, not status. Seventy-six of them at the bottom of a page called
          "what is running" is the wall of cards this screen is trying not to be. */}
          <DetailShelf summary="machine · upcoming · nodes · reservoir · entries · outcomes · inventory">
            <OperationsOverview data={data} fleet={fleet.data} error={queue.isError} />
            {outcomes.length > 0 ? (
              <RecentOutcomes rows={outcomes} now={now} loading={false} error={completed.isError} />
            ) : null}
            <QueueUpcoming />
            <ClusterCapacity nodes={nodes} loading={queue.isLoading} error={queue.isError} />
            <QueueReservoir reservoir={data?.reservoir} />
            <QueueControls />
            <QueueWorkLedger />
          </DetailShelf>

          {/* The run controls and the recovery feed both write, and both report through the shared
          toast store — which needs a stack mounted on the page to render into. */}
          <ToastStack />
        </>
      )}
    </div>
  );
}

/** The per-run controls in the page's card idiom. `RunControls` renders nothing without a live run,
 * so the card is only built when there is something to steer. */
function RunSteering() {
  const orch = useQuery(orchestrateQuery());
  const runs = orch.data?.runs ?? [];
  if (runs.length === 0) return null;
  return (
    <SectionCard eyebrow="orchestrate" title="Runs" count={String(runs.length)}>
      <div className="py-3">
        <RunControls />
      </div>
    </SectionCard>
  );
}
