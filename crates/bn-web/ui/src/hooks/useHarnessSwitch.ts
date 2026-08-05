import { useMutation, useQueryClient } from "@tanstack/react-query";
import { queryKeys } from "../api/client";
import type { HarnessOption } from "../api/types";
import { switchHarness } from "../api/writes";
import { pushToast } from "../lib/toast";

/** Fallback options when the fleet read hasn't carried availability yet — copilot always usable,
 * claude assumed usable (the server re-checks and rejects a disabled one anyway). */
export const DEFAULT_HARNESSES: HarnessOption[] = [
  { name: "copilot", available: true },
  { name: "claude", available: true },
];

/**
 * The one harness-switch code path (#501), behind every [`HarnessMenu`] — fleet cards, fleet rows
 * and the agent-detail header. Picking a different *available* harness confirms, then
 * `POST /relaunch {harness}` — bn stops the agent and relaunches it in place (same worktree/branch,
 * note re-seeded). An unavailable harness surfaces its reason rather than failing silently.
 * Optimistic toast; the fleet/activity invalidation converges it on the next tick.
 */
export function useHarnessSwitch(target: string, current?: string, harnesses?: HarnessOption[]) {
  const qc = useQueryClient();
  const options = harnesses && harnesses.length > 0 ? harnesses : DEFAULT_HARNESSES;

  const mut = useMutation({
    mutationFn: (harness: string) => switchHarness(target, harness),
    onSuccess: (_data, harness) => {
      pushToast(`restarting ${target} under ${harness}…`, "success");
      void qc.invalidateQueries({ queryKey: queryKeys.fleet });
      void qc.invalidateQueries({ queryKey: queryKeys.activityAll });
    },
    onError: (e) => {
      pushToast(`switch failed: ${e.message}`, "error");
    },
  });

  /** Attempt a switch to `opt`. No-op on the current harness or while a switch is in flight. */
  const pick = (opt: HarnessOption) => {
    if (mut.isPending || opt.name === current) return;
    if (!opt.available) {
      pushToast(opt.reason ?? `${opt.name} is unavailable`, "error");
      return;
    }
    if (
      window.confirm(
        `Relaunch ${target} under ${opt.name}? Same worktree/branch — the note is preserved.`,
      )
    ) {
      mut.mutate(opt.name);
    }
  };

  /** The button/menu-item tooltip for an option, given whether it's the running one. */
  const titleFor = (opt: HarnessOption) =>
    !opt.available
      ? (opt.reason ?? `${opt.name} is unavailable`)
      : opt.name === current
        ? `Running under ${opt.name}`
        : `Relaunch under ${opt.name}`;

  return { options, pick, titleFor, pending: mut.isPending };
}
