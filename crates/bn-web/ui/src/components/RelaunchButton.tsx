import { RotateCw } from "lucide-react";
import type { AgentControl } from "../api/types";
import { relaunchAgent } from "../api/writes";
import { useAppMutation } from "../hooks/useAppMutation";
import { affects } from "../lib/invalidation";
import { cn } from "../lib/utils";

/**
 * Relaunch an agent from the fleet board — the one repair a stuck card almost always wants.
 *
 * Relaunch means what it says: the agent's process tree is killed and the harness is started again
 * in the same worktree, resuming its own transcript. (It used to run `bn agent recover`, which
 * pushes the branch and opens a PR — nothing to do with the word on the button.)
 *
 * bn drives agents through a recorded session (`--resume`) or a live tmux window; the fleet also
 * *observes* sessions bn never launched, and those have no write channel at all. Rather than
 * failing after the click with "no such tmux window", an unsteerable agent renders the button
 * disabled and says why (same rule as [`SteerPanel`]). `undefined` control means the server predates
 * the field, so the attempt is allowed rather than locked out against a stale binary.
 *
 * Stops propagation so it works inside the card's navigation link.
 */
export function RelaunchButton({ target, control }: { target: string; control?: AgentControl }) {
  const steerable = control !== "none" && control !== "ambiguous";

  const mut = useAppMutation({
    mutationFn: () => relaunchAgent(target, false),
    invalidates: affects.agentSteer,
    toast: { success: `relaunching ${target}…`, error: "relaunch failed" },
  });

  return (
    <button
      type="button"
      disabled={!steerable || mut.isPending}
      title={
        steerable
          ? `Relaunch ${target} — kill it and start it again in the same worktree, resuming its transcript`
          : "bn has no write channel to this agent (it was not launched by bn)"
      }
      className={cn(
        "inline-flex items-center gap-1.5 rounded-md border border-border bg-muted px-2 py-1 text-xs transition-colors",
        steerable
          ? "text-muted-foreground hover:bg-accent hover:text-foreground"
          : "cursor-not-allowed text-muted-foreground/50",
      )}
      onClick={(e) => {
        e.preventDefault();
        e.stopPropagation();
        if (!steerable || mut.isPending) return;
        if (
          window.confirm(
            `Relaunch ${target}?\n\nThis kills the running agent and starts it again in the same worktree, resuming its transcript.`,
          )
        ) {
          mut.mutate();
        }
      }}
    >
      <RotateCw className={cn("h-3 w-3", mut.isPending && "animate-spin")} />
      Relaunch
    </button>
  );
}
