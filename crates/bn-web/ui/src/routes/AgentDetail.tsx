import { Link, useParams } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { AlertTriangle, ArrowLeft } from "lucide-react";
import { agentDetailQuery } from "../api/client";
import { useNow } from "../hooks/useNow";
import { AgentHeader } from "../components/AgentHeader";
import { AgentActivity } from "../components/AgentActivity";
import { SteerPanel } from "../components/SteerPanel";
import { HarnessSwitcher } from "../components/HarnessSwitcher";
import { PlanReviewPanel } from "../components/PlanReviewPanel";
import { LandButton } from "../components/LandButton";
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
 * steer → land). Header (repo/branch/health/tokens/turns/jump), the session activity feed with the
 * steer composer beneath it, a plan-review panel when the agent is paused for approval, the harness
 * switcher, and a Land control to merge its ready PR. Single-column on a phone;
 * feed-beside-controls on desktop.
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
            No live agent with session <code className="font-mono">{id}</code>. It may have
            finished — see the{" "}
            <Link to="/fleet" className="text-primary">
              Fleet
            </Link>
            .
          </p>
        </Card>
      )}

      {detail.data && agent && (
        <>
          <AgentHeader detail={detail.data} now={now} />
          <div className="grid grid-cols-1 gap-4 lg:grid-cols-[minmax(0,1fr)_22rem]">
            {/* Read the session, then reply to it — the composer sits directly under the feed it
                answers, so steering is one continuous motion instead of a hunt across the page. */}
            <div className="space-y-3">
              <AgentActivity sessionId={agent.session_id} />
              <SteerPanel target={agent.target} control={agent.control} />
            </div>
            <div className="space-y-3">
              <PlanReviewPanel repo={agent.repo} branch={agent.branch} />
              <Card className="p-4">
                <HarnessSwitcher
                  target={agent.target}
                  current={agent.harness}
                  harnesses={detail.data.harnesses}
                />
              </Card>
              <LandButton repo={agent.repo} />
            </div>
          </div>
          <SessionNote detail={detail.data} />
        </>
      )}

      <ToastStack />
    </div>
  );
}
