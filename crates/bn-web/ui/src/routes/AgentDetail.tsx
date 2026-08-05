import { Link, useParams } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { AlertTriangle, ArrowLeft } from "lucide-react";
import { agentDetailQuery, reviewsQuery } from "../api/client";
import { pendingReviewFor } from "../lib/review";
import { cn } from "../lib/utils";
import { useNow } from "../hooks/useNow";
import { AgentHeader } from "../components/AgentHeader";
import { AgentActivity } from "../components/AgentActivity";
import { SteerPanel } from "../components/SteerPanel";
import { PlanReviewPanel } from "../components/PlanReviewPanel";
import { SessionNote } from "../components/SessionNote";
import { ToastStack } from "../components/ToastStack";
import { Button } from "../components/ui/button";
import { Card } from "../components/ui/card";
import { Skeleton } from "../components/ui/skeleton";

/** The loading skeleton — header + feed placeholders so the page never flashes empty. */
function LoadingSkeleton() {
  return (
    <div className="space-y-4" aria-hidden="true">
      <Card className="space-y-3 p-4">
        <Skeleton className="h-6 w-1/2" />
        <Skeleton className="h-4 w-2/3" />
        <Skeleton className="h-4 w-1/3" />
      </Card>
      <div className="grid grid-cols-1 gap-4 lg:grid-cols-[minmax(0,1fr)_22rem]">
        <Skeleton className="h-64 w-full rounded-lg" />
        <div className="space-y-3">
          <Skeleton className="h-28 w-full rounded-lg" />
          <Skeleton className="h-40 w-full rounded-lg" />
        </div>
      </div>
    </div>
  );
}

/**
 * `/agents/:id` — one agent's detail + full steering surface (design §479 lifecycle: supervise →
 * steer). Header (repo/branch/health/harness/tokens/turns/jump), the session activity feed with the
 * steer composer beneath it, and a plan-review panel when the agent is paused for approval.
 * Single-column on a phone; feed-beside-controls on desktop.
 *
 * Landing is not here. `bn wt merge --all` is a *repo*-wide sweep — it merges every ready PR in the
 * repo, not this agent's — so a button for it on one agent's page misnames what it does; it belongs
 * to the repo surfaces. The harness moved up to the header, where it reads as this agent's identity
 * rather than a step in the steer column.
 *
 * The feed replaced the streamed xterm terminal (#491): mirroring a 200-column tmux pane into a
 * half-as-wide browser grid wrapped every line into noise, and said nothing at all about an agent
 * whose window had closed. Reading the transcript gives reflowable prose and outlives the window.
 */
export function AgentDetail() {
  const { id = "" } = useParams();
  const detail = useQuery(agentDetailQuery(id));
  const now = useNow();
  const agent = detail.data?.agent;

  // Ask the same question `PlanReviewPanel` asks itself, so the layout knows whether the aside will
  // have a body before it reserves a track for one.
  const reviews = useQuery(reviewsQuery());
  const hasPlanReview =
    !!agent && !!reviews.data && !!pendingReviewFor(reviews.data.reviews, agent.repo, agent.branch);

  return (
    <div className="space-y-5 pb-8">
      <Button asChild variant="ghost" size="sm" className="-ml-2 text-muted-foreground">
        <Link to="/fleet">
          <ArrowLeft className="h-4 w-4" /> Fleet
        </Link>
      </Button>

      {detail.isLoading && <LoadingSkeleton />}

      {detail.isError && (
        <Card className="flex flex-col items-center gap-2 px-6 py-12 text-center">
          <AlertTriangle className="h-8 w-8 text-muted-foreground" />
          <h2 className="text-lg font-semibold">Agent not live</h2>
          <p className="max-w-sm text-sm text-muted-foreground">
            No live agent with session <code className="font-mono">{id}</code>. It may have finished
            — see the{" "}
            <Link to="/fleet" className="text-primary">
              Fleet
            </Link>
            .
          </p>
        </Card>
      )}

      {detail.data && agent && (
        <>
          <AgentHeader detail={detail.data} now={now} harnesses={detail.data.harnesses} />
          {/* The aside is only a column when it has something in it. `PlanReviewPanel` renders
              nothing unless the agent is paused for approval — which is the uncommon case — so a
              hardcoded `22rem` track left the feed ending 370px short of its own header on every
              ordinary agent, for a column that was empty. */}
          <div
            className={cn(
              "grid grid-cols-1 gap-4",
              hasPlanReview && "lg:grid-cols-[minmax(0,1fr)_22rem]",
            )}
          >
            {/* Read the session, then reply to it — the composer sits directly under the feed it
                answers, so steering is one continuous motion instead of a hunt across the page. */}
            <div className="space-y-3">
              <AgentActivity sessionId={agent.session_id} />
              <SteerPanel target={agent.target} control={agent.control} />
            </div>
            {hasPlanReview && (
              <div className="space-y-3">
                <PlanReviewPanel repo={agent.repo} branch={agent.branch} />
              </div>
            )}
          </div>
          <SessionNote detail={detail.data} />
        </>
      )}

      <ToastStack />
    </div>
  );
}
