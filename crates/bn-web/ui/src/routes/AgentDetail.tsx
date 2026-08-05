import { Link, useParams } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { AlertTriangle, ArrowLeft } from "lucide-react";
import { agentDetailQuery } from "../api/client";
import { useNow } from "../hooks/useNow";
import { AgentHeader } from "../components/AgentHeader";
import { AgentTerminal } from "../components/AgentTerminal";
import { SteerPanel } from "../components/SteerPanel";
import { HarnessSwitcher } from "../components/HarnessSwitcher";
import { PlanReviewPanel } from "../components/PlanReviewPanel";
import { LandButton } from "../components/LandButton";
import { SessionNote } from "../components/SessionNote";
import { ToastStack } from "../components/ToastStack";
import { Button } from "../components/ui/button";
import { Card } from "../components/ui/card";
import { Skeleton } from "../components/ui/skeleton";

/** The loading skeleton — header + terminal placeholders so the page never flashes empty. */
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
 * steer → land). Header (repo/branch/health/tokens/turns/jump), the live streamed terminal (#491 —
 * a WebSocket into the agent's tmux pane, with token-gated "take control" input), a plan-review
 * panel when the agent is paused for approval, the steer composer + relaunch/kill controls, and a
 * Land control to merge its ready PR. Single-column on a phone; terminal-beside-controls on desktop.
 */
export function AgentDetail() {
  const { id = "" } = useParams();
  const detail = useQuery(agentDetailQuery(id));
  const now = useNow();
  const agent = detail.data?.agent;

  return (
    <div className="mx-auto max-w-5xl space-y-5 pb-8">
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
            <AgentTerminal target={agent.target} />
            <div className="space-y-3">
              <PlanReviewPanel repo={agent.repo} branch={agent.branch} />
              <Card className="p-4">
                <HarnessSwitcher
                  target={agent.target}
                  current={agent.harness}
                  harnesses={detail.data.harnesses}
                />
              </Card>
              <SteerPanel target={agent.target} />
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
