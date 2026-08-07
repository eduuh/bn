import { useState, type ReactNode } from "react";
import { useQuery } from "@tanstack/react-query";
import {
  ArrowRight,
  Check,
  ExternalLink,
  MessageSquareText,
  NotebookText,
  Pause,
  Play,
} from "lucide-react";
import { Link } from "react-router-dom";

import { completedQuery, fleetQuery, queryKeys, queueQuery, reviewsQuery } from "../api/client";
import { decideReview, queuePause, queueResume, sendMessage } from "../api/writes";
import type { QueueDeferredRow, QueueNodeRow, QueueView } from "../api/queue-types";
import type { CompletedItem, FleetView, LiveAgent, Review } from "../api/types";
import { QueueControls } from "../components/QueueControls";
import { QueueReservoir } from "../components/QueueReservoir";
import { QueueUpcoming } from "../components/QueueUpcoming";
import { QueueWorkLedger } from "../components/QueueWorkLedger";
import { Empty, LoadError, RowsSkeleton, SectionCard } from "../components/SectionCard";
import { SteerButton } from "../components/SteerButton";
import { Badge, type BadgeProps } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import { Card } from "../components/ui/card";
import { useAppMutation, optimistic } from "../hooks/useAppMutation";
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
          detail={data.paused ? "admission paused" : "accepting work"}
          tone={machine?.dispatcher_alive ? "text-ok" : "text-urgent"}
        />
        <Metric
          label="live agents"
          value={fleet?.agents.length ?? "—"}
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

function ActiveAgents({
  agents,
  now,
  loading,
  error,
}: {
  agents: LiveAgent[];
  now: number;
  loading: boolean;
  error: boolean;
}) {
  return (
    <SectionCard eyebrow="active now" title="Working agents" count={String(agents.length)}>
      {error ? (
        <LoadError>Could not load the live fleet.</LoadError>
      ) : loading ? (
        <RowsSkeleton rows={3} />
      ) : agents.length === 0 ? (
        <Empty>No agents are working right now.</Empty>
      ) : (
        <ul className="divide-y divide-border">
          {agents.map((agent) => (
            <AgentRow key={agent.session_id} agent={agent} now={now} />
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

function Deferred({
  rows,
  loading,
  error,
}: {
  rows: QueueDeferredRow[];
  loading: boolean;
  error: boolean;
}) {
  return (
    <SectionCard eyebrow="held" title="Not started" count={String(rows.length)}>
      {error ? (
        <LoadError>Could not load the dispatcher decision.</LoadError>
      ) : loading ? (
        <RowsSkeleton rows={2} />
      ) : rows.length === 0 ? (
        <Empty>Nothing is being held back.</Empty>
      ) : (
        <ul className="divide-y divide-border">
          {rows.map((row) => (
            <li
              key={`${row.repo}/${row.id}`}
              className="flex flex-wrap items-baseline gap-x-2 gap-y-1 py-2 text-sm"
            >
              <Badge variant={row.reason === "paused" ? "warn" : "muted"}>{row.reason}</Badge>
              <span className="font-mono text-xs text-muted-foreground">
                {row.repo}/{row.id}
              </span>
              <span className="text-muted-foreground">{row.kind}</span>
              {row.detail ? <span className="text-foreground/80">— {row.detail}</span> : null}
            </li>
          ))}
        </ul>
      )}
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

export function Queue() {
  const queue = useQuery(queueQuery());
  const fleet = useQuery(fleetQuery());
  const reviews = useQuery(reviewsQuery());
  const completed = useQuery(completedQuery());
  const [reason, setReason] = useState("");

  const setFullyPaused = <V,>(fullyPaused: boolean) =>
    optimistic<QueueView, V>(queryKeys.queue, (view) => {
      const pauses = fullyPaused
        ? view.pauses
        : view.pauses.filter((pause) => pause.scope.scope !== "all");
      return {
        ...view,
        paused: fullyPaused || pauses.length > 0,
        fully_paused: fullyPaused,
        pauses,
      };
    });

  const pause = useAppMutation<unknown, { scope: string; reason?: string }>({
    mutationFn: (body) => queuePause(body.scope, body.reason),
    optimistic: setFullyPaused<{ scope: string; reason?: string }>(true),
    invalidates: affects.queue,
    toast: { success: "Queue paused", error: "Could not pause" },
  });
  const resume = useAppMutation<unknown, { scope: string }>({
    mutationFn: (body) => queueResume(body.scope),
    optimistic: setFullyPaused<{ scope: string }>(false),
    invalidates: affects.queue,
    toast: { success: "Queue resumed", error: "Could not resume" },
  });

  const data = queue.data;
  const agents = fleet.data?.agents ?? [];
  const fullyPaused = data?.fully_paused ?? false;
  const pauseBusy = pause.isPending || resume.isPending;
  const now = data?.now ?? Math.floor(Date.now() / 1000);

  return (
    <div className="mx-auto w-full max-w-6xl space-y-4 px-4 py-5">
      <header className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <h1 className="text-xl font-semibold">Queue</h1>
          <p className="text-sm text-muted-foreground">
            Live work, decisions, capacity, outcomes, and the complete durable inventory.
          </p>
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
          {!fullyPaused ? (
            <input
              value={reason}
              onChange={(event) => setReason(event.target.value)}
              placeholder="why pause? (optional)"
              aria-label="Pause reason"
              className="h-8 w-52 rounded-md border border-border bg-background px-2 text-sm"
            />
          ) : null}
          <Button
            size="sm"
            variant={fullyPaused ? "default" : "secondary"}
            disabled={pauseBusy}
            onClick={() =>
              fullyPaused
                ? resume.mutate({ scope: "all" })
                : pause.mutate({ scope: "all", reason: reason.trim() || undefined })
            }
          >
            {fullyPaused ? <Play /> : <Pause />}
            {fullyPaused ? "Resume" : "Pause"}
          </Button>
        </div>
      </header>

      <OperationsOverview data={data} fleet={fleet.data} error={queue.isError} />

      {data?.paused ? (
        <Card className="border-warn/40 bg-warn/5 p-3 text-sm">
          <p className="font-medium text-warn">
            {data.fully_paused
              ? "The dispatcher is not starting new work."
              : "Some queue scopes are paused."}
          </p>
          {data.pauses.map((entry, index) => (
            <p key={`${entry.scope.scope}:${index}`} className="text-muted-foreground">
              {entry.reason ? `“${entry.reason}”` : "No reason given"}
              {entry.until ? ` · until ${new Date(entry.until * 1000).toLocaleTimeString()}` : ""}
            </p>
          ))}
          <p className="text-muted-foreground">
            Running agents finish normally; a pause only stops new admission.
          </p>
        </Card>
      ) : null}

      <ActiveAgents agents={agents} now={now} loading={fleet.isLoading} error={fleet.isError} />
      <QueueUpcoming />
      <NeedsInput
        reviews={reviews.data?.reviews ?? []}
        agents={agents}
        now={now}
        loading={reviews.isLoading || fleet.isLoading}
        error={reviews.isError || fleet.isError}
      />
      <ClusterCapacity nodes={data?.nodes ?? []} loading={queue.isLoading} error={queue.isError} />
      <QueueReservoir reservoir={data?.reservoir} />
      <QueueControls />
      <Deferred
        rows={data?.decision?.deferred ?? []}
        loading={queue.isLoading}
        error={queue.isError}
      />
      <QueueShape data={data} />
      <RecentOutcomes
        rows={completed.data?.completed ?? []}
        now={now}
        loading={completed.isLoading}
        error={completed.isError}
      />
      <QueueWorkLedger />
    </div>
  );
}
