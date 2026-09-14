/** The repo page's operational controls: restart everything.
 *
 * Split out of `RepoDetail` at the 1000-line limit, which is the right seam anyway: this is the
 * only part of the page that changes the machine rather than describing it.
 */
import { RefreshCw } from "lucide-react";

import { relaunchRepoSession } from "../api/writes";
import { useAppMutation } from "../hooks/useAppMutation";
import { affects } from "../lib/invalidation";
import { ConfirmButton } from "./ConfirmButton";
import { SectionHead } from "./SectionHead";
import { Card } from "./ui/card";

export function RepoControls({ repo, live }: { repo: string; live: number }) {
  return <RelaunchPanel repo={repo} live={live} />;
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
