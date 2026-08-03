import { useMutation, useQueryClient } from "@tanstack/react-query";
import { RotateCw } from "lucide-react";
import { queryKeys } from "../api/client";
import type { AgentControl } from "../api/types";
import { relaunchAgent } from "../api/writes";
import { pushToast } from "../lib/toast";
import { cn } from "../lib/utils";

/**
 * Soft-relaunch an agent from the fleet board — the one repair a stuck card almost always wants,
 * previously reachable only by opening the agent detail.
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
  const qc = useQueryClient();
  const steerable = control !== "none" && control !== "ambiguous";

  const mut = useMutation({
    mutationFn: () => relaunchAgent(target, false),
    onSuccess: () => {
      pushToast(`relaunching ${target}…`, "success");
      void qc.invalidateQueries({ queryKey: queryKeys.fleet });
      void qc.invalidateQueries({ queryKey: queryKeys.activityAll });
    },
    onError: (e) => pushToast(`relaunch failed: ${(e as Error).message}`, "error"),
  });

  return (
    <button
      type="button"
      disabled={!steerable || mut.isPending}
      title={
        steerable
          ? `Relaunch ${target} — same worktree and branch, note preserved`
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
        if (window.confirm(`Relaunch ${target}? Same worktree and branch — the note is preserved.`)) {
          mut.mutate();
        }
      }}
    >
      <RotateCw className={cn("h-3 w-3", mut.isPending && "animate-spin")} />
      Relaunch
    </button>
  );
}
