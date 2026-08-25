/** The repo page's operational controls: hold intake and restart everything.
 *
 * Together rather than scattered, and in that order, because the order *is* the guidance — each one
 * costs strictly more than the one above it, so the cheapest thing that could work is always the
 * first thing you reach. Split out of `RepoDetail` at the 1000-line limit, which is the right seam
 * anyway: this is the only part of the page that changes the machine rather than describing it.
 */
import { useQuery } from "@tanstack/react-query";
import { PauseCircle, PlayCircle, RefreshCw } from "lucide-react";

import { queueQuery } from "../api/client";
import { drainRepo, relaunchRepoSession } from "../api/writes";
import { useAppMutation } from "../hooks/useAppMutation";
import { affects } from "../lib/invalidation";
import { ConfirmButton } from "./ConfirmButton";
import { SectionHead } from "./SectionHead";
import { Badge } from "./ui/badge";
import { Button } from "./ui/button";
import { Card } from "./ui/card";

export function RepoControls({ repo, live }: { repo: string; live: number }) {
  return (
    <>
      <DrainPanel repo={repo} />
      <RelaunchPanel repo={repo} live={live} />
    </>
  );
}

/**
 * Hold new work for this repo, letting whatever is running finish — the drain.
 *
 * The non-destructive sibling of the two buttons below it, and usually the one that is actually
 * wanted: "make this repo go quiet" almost never means "kill what it is doing". So it takes no
 * confirm — there is nothing to lose, and pressing it again is the whole undo.
 *
 * Drained state is read from the queue's pause list rather than tracked here, because the drain is
 * machine state: a repo paused from the CLI must show as drained on this page, and a page that kept
 * its own copy would eventually disagree with the dispatcher about whether work is flowing.
 */
function DrainPanel({ repo }: { repo: string }) {
  const queue = useQuery(queueQuery());
  const scope = `repo:${repo.toLowerCase()}`;
  const pause = queue.data?.pauses?.find((p) => p.scope.toLowerCase() === scope);
  const drained = pause != null;
  const drain = useAppMutation({
    mutationFn: () => drainRepo(repo, { resume: drained }),
    invalidates: affects.agentLaunch,
    toast: {
      success: () =>
        drained
          ? `${repo} is accepting work again`
          : `${repo} is draining — running agents finish, nothing new starts`,
      error: `Could not ${drained ? "resume" : "drain"} ${repo}`,
    },
  });

  return (
    <section className="space-y-3">
      <SectionHead icon={drained ? PauseCircle : PlayCircle} title="Work intake" />
      <Card className="flex flex-wrap items-center gap-3 p-4">
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2 text-sm font-semibold">
            {drained ? "Draining" : "Accepting work"}
            {drained && <Badge variant="warn">paused</Badge>}
          </div>
          <p className="text-xs text-muted-foreground">
            {drained
              ? "The queue is holding new work for this repo. Agents already running finish normally."
              : "Stop the queue starting new work here, without touching what is already running."}
          </p>
        </div>
        <Button
          type="button"
          variant="secondary"
          size="sm"
          disabled={drain.isPending}
          onClick={() => drain.mutate()}
        >
          {drained ? (
            <>
              <PlayCircle className="h-4 w-4" /> Resume intake
            </>
          ) : (
            <>
              <PauseCircle className="h-4 w-4" /> Drain repo
            </>
          )}
        </Button>
      </Card>
    </section>
  );
}

/**
 * Restart every agent in this repo, each in the worktree it was already in.
 *
 * Sits *above* the force kill because it is almost always the move people actually want: a wedged
 * session is a thing to repair, not to lose. A soft relaunch resumes each agent's recorded
 * transcript, so their context survives — which is the one fact the confirm has to carry, since it
 * is the whole difference from the button below it.
 */
function RelaunchPanel({ repo, live }: { repo: string; live: number }) {
  const relaunch = useAppMutation({
    mutationFn: () => relaunchRepoSession(repo),
    invalidates: affects.agentLaunch,
    toast: {
      success: (r) => {
        const done = (r as { relaunched?: unknown[] })?.relaunched?.length ?? 0;
        const failed = (r as { failed?: unknown[] })?.failed?.length ?? 0;
        if (done === 0 && failed === 0) return `No live agents in ${repo} to restart`;
        // A partial restart is reported as a partial restart: rolling it up to "restarted N" would
        // hide the agents that are now simply gone.
        return failed > 0
          ? `Restarted ${done} in ${repo}, ${failed} did not come back`
          : `Restarted ${done} agent${done === 1 ? "" : "s"} in ${repo}`;
      },
      error: `Could not restart the agents in ${repo}`,
    },
  });

  if (live === 0) return null;
  return (
    <section className="space-y-3">
      <SectionHead icon={RefreshCw} title="Restart agents" />
      <Card className="flex flex-wrap items-center gap-3 p-4">
        <div className="min-w-0 flex-1">
          <div className="text-sm font-semibold">Restart every agent in this repo</div>
          <p className="text-xs text-muted-foreground">
            Each agent is stopped and started again in the same worktree, resuming its transcript —
            the repair for a session that is stuck rather than finished.
          </p>
        </div>
        <ConfirmButton
          variant="secondary"
          size="sm"
          disabled={relaunch.isPending}
          title={`Restart the agents in ${repo}?`}
          description={
            <>
              This stops and restarts{" "}
              <strong>
                {live} agent{live === 1 ? "" : "s"}
              </strong>{" "}
              in <code className="font-mono">{repo}</code>. Each one resumes its recorded transcript
              in the same worktree, so their context is kept — unlike a force kill.
            </>
          }
          confirmLabel={relaunch.isPending ? "restarting…" : "Restart"}
          onConfirm={() => relaunch.mutate()}
        >
          <RefreshCw className="h-4 w-4" /> Restart agents
        </ConfirmButton>
      </Card>
    </section>
  );
}
