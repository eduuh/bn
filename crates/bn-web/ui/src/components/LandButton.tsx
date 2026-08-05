import { useMutation, useQueryClient } from "@tanstack/react-query";
import { Download } from "lucide-react";
import { queryKeys } from "../api/client";
import { apiLand } from "../api/writes";
import { pushToast } from "../lib/toast";
import { Button } from "./ui/button";

/** Land control (design §479 lifecycle end): trigger `bn wt merge --all` for the agent's repo to
 * merge its ready PR. Confirms first, reports via toast, and refreshes the fleet/orchestrate feeds
 * so a merged item moves out of flight without a reload. */
export function LandButton({ repo }: { repo: string }) {
  const qc = useQueryClient();
  const mut = useMutation({
    mutationFn: () => apiLand(repo),
    onSuccess: () => {
      pushToast(`Landing ready PRs for ${repo}…`, "success");
      void qc.invalidateQueries({ queryKey: queryKeys.orchestrate });
      void qc.invalidateQueries({ queryKey: queryKeys.fleet });
    },
    onError: (e) => {
      pushToast(`land failed: ${(e as Error).message}`, "error");
    },
  });
  return (
    <Button
      type="button"
      variant="outline"
      className="w-full"
      disabled={mut.isPending}
      onClick={() => {
        if (window.confirm(`Land ready PRs for ${repo}? Runs \`bn wt merge --all\`.`)) mut.mutate();
      }}
    >
      <Download className="h-4 w-4" /> Land ready PRs
    </Button>
  );
}
